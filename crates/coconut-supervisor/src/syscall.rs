//! syscall/sysret support via x86-64 MSRs.
//!
//! Sets up EFER.SCE, STAR, LSTAR, SFMASK so that `syscall` from ring 3
//! enters `syscall_entry` in ring 0. The entry stub saves user context,
//! dispatches to Rust, and returns via `sysretq`.

use core::arch::{asm, naked_asm};

use crate::gdt;
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
        //
        // On syscall: CPU loads CS = STAR[47:32], SS = STAR[47:32] + 8
        //   → CS = 0x08 (kernel code), SS = 0x10 (kernel data)
        //
        // On sysret: CPU loads CS = STAR[63:48] + 16, SS = STAR[63:48] + 8
        //   → With base 0x10: CS = 0x10+16 = 0x20 (user code, entry 4), SS = 0x10+8 = 0x18 (user data, entry 3)
        //   → But RPL=3 is forced by sysret, so effective CS = 0x23, SS = 0x1B
        //
        // Wait — sysret in long mode: CS = STAR[63:48] + 16 with RPL forced to 3
        //   SS = STAR[63:48] + 8 with RPL forced to 3
        //   So STAR[63:48] should be 0x10 to get:
        //     CS = 0x10 + 16 = 0x20 → user code (index 4) with RPL=3 → 0x23
        //     SS = 0x10 + 8  = 0x18 → user data (index 3) with RPL=3 → 0x1B
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
/// For SYS_EXIT, this function does not return (it longjmps back to shard::run).
#[no_mangle]
extern "C" fn syscall_dispatch(nr: u64, a0: u64, a1: u64, _a2: u64) -> u64 {
    match nr {
        coconut_shared::SYS_EXIT => {
            shard::handle_sys_exit(a0);
            // does not return
        }
        coconut_shared::SYS_SERIAL_WRITE => {
            handle_serial_write(a0, a1)
        }
        _ => {
            crate::serial_println!("Unknown syscall: {}", nr);
            u64::MAX // error
        }
    }
}

/// Handle SYS_SERIAL_WRITE: validate buffer is in shard address range, then print.
fn handle_serial_write(buf_ptr: u64, len: u64) -> u64 {
    // Validate: buffer must be within shard code/data region [0x1000, 0x2000)
    if buf_ptr < 0x1000 || buf_ptr + len > 0x2000 || len > 4096 {
        return u64::MAX;
    }

    // The buffer is at a user-space virtual address. Since we're in the syscall
    // handler, CR3 is still the shard's page table (which includes supervisor
    // mappings). We can read from the user virtual address.
    let buf = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, len as usize) };

    for &byte in buf {
        if byte == b'\n' {
            crate::serial::write_byte(b'\r');
        }
        crate::serial::write_byte(byte);
    }

    0 // success
}
