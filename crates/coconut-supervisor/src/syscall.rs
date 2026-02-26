//! syscall/sysret support via x86-64 MSRs.
//!
//! Sets up EFER.SCE, STAR, LSTAR, SFMASK so that `syscall` from ring 3
//! enters `syscall_entry` in ring 0. The entry stub saves user context,
//! dispatches to Rust, and returns via `sysretq`.

use core::arch::{asm, naked_asm};

use crate::channel;
use crate::gdt;
use crate::scheduler;
use crate::shard;

// MSR addresses
const IA32_EFER: u32 = 0xC000_0080;
const IA32_STAR: u32 = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_SFMASK: u32 = 0xC000_0084;

// EFER bits
const EFER_SCE: u64 = 1 << 0; // System Call Extensions

// RFLAGS bits to mask on syscall entry
const SFMASK_VALUE: u64 = 0x200 | 0x100; // Clear IF (interrupts) and TF (trap)

/// Saved kernel RSP for the syscall entry stub.
/// Set before entering ring 3, used by the stub to switch to kernel stack.
pub static mut KERNEL_RSP: u64 = 0;

/// Saved user RSP during syscall handling.
static mut USER_RSP: u64 = 0;

/// Read an MSR.
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Write an MSR.
unsafe fn wrmsr(msr: u32, value: u64) {
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// Initialize syscall/sysret MSRs.
pub fn init() {
    unsafe {
        // Enable SCE (System Call Extensions) in EFER
        let efer = rdmsr(IA32_EFER);
        wrmsr(IA32_EFER, efer | EFER_SCE);

        // STAR: bits [47:32] = kernel CS (for syscall), bits [63:48] = user CS base (for sysret)
        let kernel_cs = gdt::KERNEL_CS as u64;
        let sysret_base = gdt::KERNEL_DS as u64; // 0x10
        let star = (sysret_base << 48) | (kernel_cs << 32);
        wrmsr(IA32_STAR, star);

        // LSTAR: syscall entry point (RIP on syscall)
        let entry_addr = syscall_entry as *const () as u64;
        wrmsr(IA32_LSTAR, entry_addr);

        // SFMASK: RFLAGS bits to clear on syscall entry
        wrmsr(IA32_SFMASK, SFMASK_VALUE);
    }
}

/// Syscall entry stub.
///
/// On `syscall`:
///   - RCX = user RIP (return address)
///   - R11 = user RFLAGS
///   - RSP is still user RSP (not switched by hardware!)
///   - RDI, RSI, RDX = syscall args (System V convention from user code)
///   - RAX = syscall number
#[unsafe(naked)]
#[no_mangle]
unsafe extern "C" fn syscall_entry() {
    naked_asm!(
        // Save user RSP, load kernel RSP
        "mov [{user_rsp}], rsp",
        "mov rsp, [{kernel_rsp}]",

        // Push user context for restore after syscall
        "push rcx",             // user RIP
        "push r11",             // user RFLAGS

        // Save callee-saved registers that Rust might clobber
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // Set up arguments for syscall_dispatch(nr, a0, a1, a2)
        //   RAX = syscall number → RDI (arg0)
        //   RDI = arg0 → RSI (arg1)
        //   RSI = arg1 → RDX (arg2)
        //   RDX = arg2 → RCX (arg3)
        "mov rcx, rdx",        // arg2 → rcx
        "mov rdx, rsi",        // arg1 → rdx
        "mov rsi, rdi",        // arg0 → rsi
        "mov rdi, rax",        // syscall nr → rdi

        "call {dispatch}",

        // Return value in RAX is already set by dispatch

        // Restore callee-saved registers
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",

        // Restore user context
        "pop r11",              // user RFLAGS
        "pop rcx",              // user RIP

        // Restore user RSP
        "mov rsp, [{user_rsp}]",

        "sysretq",

        user_rsp = sym USER_RSP,
        kernel_rsp = sym KERNEL_RSP,
        dispatch = sym syscall_dispatch,
    );
}

/// Rust syscall dispatcher.
///
/// Returns a result value in RAX (0 = success for most calls).
/// For SYS_EXIT, this function does not return normally — it context-switches
/// to the supervisor via schedule_yield_exit.
#[no_mangle]
extern "C" fn syscall_dispatch(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    match nr {
        coconut_shared::SYS_EXIT => {
            shard::handle_sys_exit(a0);
            // does not return
            unreachable!()
        }
        coconut_shared::SYS_SERIAL_WRITE => handle_serial_write(a0, a1),
        coconut_shared::SYS_CHANNEL_SEND => handle_channel_send(a0, a1, a2),
        coconut_shared::SYS_CHANNEL_RECV => handle_channel_recv(a0, a1, a2),
        coconut_shared::SYS_YIELD => {
            scheduler::handle_sys_yield();
            0
        }
        _ => {
            crate::serial_println!("Unknown syscall: {}", nr);
            u64::MAX // error
        }
    }
}

/// Validate that a user buffer [ptr, ptr+len) lies entirely within
/// allowed user-space regions: code [0x1000, 0x2000) or stack [0x7FF000, 0x800000).
fn validate_user_read_buf(ptr: u64, len: u64) -> bool {
    if len == 0 || len > 4096 {
        return false;
    }
    let end = ptr.wrapping_add(len);
    if end < ptr {
        return false; // overflow
    }
    // Code region
    if ptr >= 0x1000 && end <= 0x2000 {
        return true;
    }
    // Stack region
    if ptr >= 0x7FF000 && end <= 0x800000 {
        return true;
    }
    false
}

/// Validate that a user buffer for writing lies in the stack region [0x7FF000, 0x800000).
fn validate_user_write_buf(ptr: u64, len: u64) -> bool {
    if len == 0 || len > 4096 {
        return false;
    }
    let end = ptr.wrapping_add(len);
    if end < ptr {
        return false;
    }
    ptr >= 0x7FF000 && end <= 0x800000
}

/// Handle SYS_SERIAL_WRITE: validate buffer is in shard address range, then print.
fn handle_serial_write(buf_ptr: u64, len: u64) -> u64 {
    if !validate_user_read_buf(buf_ptr, len) {
        return u64::MAX;
    }

    let buf = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, len as usize) };

    for &byte in buf {
        if byte == b'\n' {
            crate::serial::write_byte(b'\r');
        }
        crate::serial::write_byte(byte);
    }

    0
}

/// Handle SYS_CHANNEL_SEND: send a message on a channel.
fn handle_channel_send(channel_id: u64, buf_ptr: u64, len: u64) -> u64 {
    if channel_id as usize >= channel::MAX_CHANNELS {
        return u64::MAX;
    }
    if len as usize > channel::MAX_MSG_SIZE || len == 0 {
        return u64::MAX;
    }
    if !validate_user_read_buf(buf_ptr, len) {
        return u64::MAX;
    }

    let sender = shard::current_shard();
    channel::send(
        channel_id as usize,
        sender,
        buf_ptr as *const u8,
        len as usize,
    )
}

/// Handle SYS_CHANNEL_RECV: receive a message from a channel (may block).
fn handle_channel_recv(channel_id: u64, buf_ptr: u64, max_len: u64) -> u64 {
    if channel_id as usize >= channel::MAX_CHANNELS {
        return u64::MAX;
    }
    if max_len == 0 || max_len as usize > channel::MAX_MSG_SIZE {
        return u64::MAX;
    }
    if !validate_user_write_buf(buf_ptr, max_len) {
        return u64::MAX;
    }

    let receiver = shard::current_shard();
    channel::recv(
        channel_id as usize,
        receiver,
        buf_ptr as *mut u8,
        max_len as usize,
    )
}
