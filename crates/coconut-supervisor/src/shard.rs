//! Shard lifecycle: create, run, and destroy isolated user-mode address spaces.
//!
//! A shard is an isolated execution context with its own page tables (PML4),
//! user-mode code, and stack. The supervisor copies its upper-half PML4 entries
//! into the shard so syscalls/interrupts can access kernel code.

use core::arch::asm;

use crate::frame;
use crate::gdt;
use crate::highhalf;
use crate::syscall;
use crate::tss;
use crate::vmm::{self, PTE_PRESENT, PTE_WRITABLE, PTE_USER, PTE_NO_EXECUTE};

/// Maximum number of frames a single shard can allocate.
const MAX_SHARD_FRAMES: usize = 32;

/// Virtual address where the shard code is mapped.
const SHARD_CODE_VADDR: u64 = 0x1000;

/// Virtual address where the shard stack page is mapped (top of stack at 0x800000).
const SHARD_STACK_VADDR: u64 = 0x7FF000; // one 4K page, stack top = 0x800000

/// Initial stack pointer for the shard (top of stack page).
const SHARD_INITIAL_RSP: u64 = 0x800000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShardState {
    Free,
    Active,
    Exited,
    Destroyed,
}

struct ShardDescriptor {
    state: ShardState,
    pml4_phys: u64,
    allocated_frames: [u64; MAX_SHARD_FRAMES],
    frame_count: usize,
    exit_code: u64,
}

const MAX_SHARDS: usize = 1; // single shard for 0.2

static mut SHARDS: [ShardDescriptor; MAX_SHARDS] = [const {
    ShardDescriptor {
        state: ShardState::Free,
        pml4_phys: 0,
        allocated_frames: [0; MAX_SHARD_FRAMES],
        frame_count: 0,
        exit_code: 0,
    }
}; MAX_SHARDS];

/// Currently running shard ID (or usize::MAX if none).
static mut CURRENT_SHARD: usize = usize::MAX;

// ---------------------------------------------------------------------------
// Embedded test shard binary (flat binary, runs at 0x1000 in ring 3)
//
// This tiny program:
//   1. Writes "Hello from shard!\n" via SYS_SERIAL_WRITE (syscall 1)
//   2. Exits via SYS_EXIT (syscall 0) with code 0
// ---------------------------------------------------------------------------
core::arch::global_asm!(
    ".section .rodata",
    ".balign 16",
    ".global _test_shard_start",
    ".global _test_shard_end",
    "_test_shard_start:",

    // Position-independent code that will run at 0x1000
    // SYS_SERIAL_WRITE(buf=msg, len=18)
    "lea rdi, [rip + 2f]",       // buf pointer (rdi = arg0 for syscall ABI)
    "mov rsi, 18",               // len (rsi = arg1) — "Hello from shard!\n" = 18 bytes
    "mov rax, 1",                // SYS_SERIAL_WRITE
    "syscall",

    // SYS_EXIT(0)
    "xor edi, edi",              // exit code 0
    "mov rax, 0",                // SYS_EXIT
    "syscall",

    // Hang if syscall returns (shouldn't happen)
    "1: hlt",
    "jmp 1b",

    // Message string
    "2: .ascii \"Hello from shard!\\n\"",

    "_test_shard_end:",
);

extern "C" {
    static _test_shard_start: u8;
    static _test_shard_end: u8;
}

/// Create a new shard with the embedded test binary.
/// Returns the shard ID (index).
pub fn create() -> usize {
    let id = 0;
    crate::serial_println!("Shard {}: creating...", id);

    let shard = unsafe { &mut (*(&raw mut SHARDS))[id] };
    assert!(shard.state == ShardState::Free, "shard slot not free");

    // 1. Allocate PML4 for the shard
    let pml4_phys = frame::alloc_frame_zeroed().expect("shard: failed to alloc PML4");
    shard.pml4_phys = pml4_phys;
    shard.allocated_frames[0] = pml4_phys;
    shard.frame_count = 1;

    // 2. Copy supervisor's upper-half PML4 entries (256-511) into shard PML4
    //    This shares the kernel mappings (HHDM + kernel code/data) with the shard,
    //    but the USER bit is not set, so ring 3 can't access them.
    let sup_pml4_phys = highhalf::supervisor_pml4();
    let sup_pml4 = vmm::phys_to_virt(sup_pml4_phys) as *const u64;
    let shard_pml4 = vmm::phys_to_virt(pml4_phys) as *mut u64;
    for i in 256..512 {
        unsafe {
            let entry = core::ptr::read_volatile(sup_pml4.add(i));
            core::ptr::write_volatile(shard_pml4.add(i), entry);
        }
    }

    // 3. Allocate code frame, copy test shard binary, map at 0x1000 (R+X, USER)
    let code_phys = frame::alloc_frame_zeroed().expect("shard: failed to alloc code frame");
    shard.allocated_frames[shard.frame_count] = code_phys;
    shard.frame_count += 1;

    let code_src = (&raw const _test_shard_start) as *const u8;
    let code_end = (&raw const _test_shard_end) as *const u8;
    let code_size = code_end as usize - code_src as usize;
    let code_dest = vmm::phys_to_virt(code_phys);
    unsafe {
        core::ptr::copy_nonoverlapping(code_src, code_dest, code_size);
    }

    // Map code page: PRESENT | USER (readable + executable, not writable, not NX)
    vmm::map_4k(pml4_phys, SHARD_CODE_VADDR, code_phys, PTE_USER);

    // 4. Allocate stack frame (zeroed), map at 0x7FF000 (R+W, USER, NX)
    let stack_phys = frame::alloc_frame_zeroed().expect("shard: failed to alloc stack frame");
    shard.allocated_frames[shard.frame_count] = stack_phys;
    shard.frame_count += 1;

    vmm::map_4k(pml4_phys, SHARD_STACK_VADDR, stack_phys,
                PTE_USER | PTE_WRITABLE | PTE_NO_EXECUTE);

    shard.state = ShardState::Active;

    crate::serial_println!(
        "Shard {}: code at virt {:#x} (R+X), stack at virt {:#x} (R+W+NX)",
        id, SHARD_CODE_VADDR, SHARD_STACK_VADDR
    );

    id
}

/// Enter ring 3 and run the shard.
///
/// This function returns when the shard calls SYS_EXIT. The exit path in
/// handle_sys_exit() longjmps back here by restoring the saved kernel state.
pub fn run(id: usize) -> ! {
    let shard = unsafe { &(*(&raw const SHARDS))[id] };
    assert!(shard.state == ShardState::Active, "shard not active");

    crate::serial_println!("Shard {}: entering ring 3...", id);

    unsafe {
        *(&raw mut CURRENT_SHARD) = id;
    }

    // Set TSS.RSP0 and syscall kernel RSP to kernel stack top
    extern "C" {
        static __stack_top: u8;
    }
    let kernel_stack_top = (&raw const __stack_top) as u64;
    tss::set_rsp0(kernel_stack_top);
    unsafe {
        *(&raw mut syscall::KERNEL_RSP) = kernel_stack_top;
    }

    // Switch to shard's page tables
    vmm::write_cr3(shard.pml4_phys);

    // Enter ring 3 via sysretq
    //   RCX = user RIP (entry point)
    //   R11 = user RFLAGS (IF=1 for interrupts)
    //   RSP = user stack pointer
    // We also need to set the data segment registers to user DS
    let user_ds = gdt::USER_DS as u64;
    unsafe {
        asm!(
            // Set user data segments
            "mov ds, {ds:x}",
            "mov es, {ds:x}",

            // Set user RSP
            "mov rsp, {user_rsp}",

            // RCX = user RIP, R11 = user RFLAGS
            "mov rcx, {user_rip}",
            "mov r11, {user_rflags}",

            "sysretq",

            ds = in(reg) user_ds,
            user_rsp = in(reg) SHARD_INITIAL_RSP,
            user_rip = in(reg) SHARD_CODE_VADDR,
            user_rflags = in(reg) 0x202u64, // IF=1, reserved bit 1
            options(noreturn),
        );
    }
}

/// Handle SYS_EXIT from the current shard.
///
/// This is called from syscall_dispatch. It switches back to the supervisor's
/// page tables and returns to supervisor_main by restoring kernel state.
pub fn handle_sys_exit(exit_code: u64) -> ! {
    let id = unsafe { *(&raw const CURRENT_SHARD) };

    crate::serial_println!("Shard {}: sys_exit({})", id, exit_code);

    unsafe {
        let shard = &mut (*(&raw mut SHARDS))[id];
        shard.exit_code = exit_code;
        shard.state = ShardState::Exited;
        *(&raw mut CURRENT_SHARD) = usize::MAX;
    }

    // Switch back to supervisor page tables
    vmm::write_cr3(highhalf::supervisor_pml4());

    // Restore kernel data segments
    let kernel_ds = gdt::KERNEL_DS as u64;
    unsafe {
        asm!(
            "mov ds, {0:x}",
            "mov es, {0:x}",
            in(reg) kernel_ds,
            options(nostack, preserves_flags),
        );
    }

    // Return to supervisor_main.
    // The shard::run() function called sysretq with noreturn, so we can't
    // return normally. Instead, jump back to the code after shard::run() returns
    // in supervisor_main. We do this by returning to the caller of run() —
    // since run() used noreturn, we need to call supervisor_main's continuation.
    //
    // Simplest approach: just call back into the post-shard code in main.
    // The supervisor_main function expects run() to "return", so we simulate
    // that by returning to the address saved by syscall_entry on the kernel stack.
    //
    // Actually, since syscall_entry saved state on the kernel stack and called
    // syscall_dispatch which called us, we can just... not return from this
    // function and instead manually restore the kernel stack and jump to the
    // post-shard continuation.
    //
    // The cleanest approach for 0.2: use a saved continuation point.
    extern "C" {
        static __stack_top: u8;
    }
    let stack_top = (&raw const __stack_top) as u64;

    // Jump to the post-shard handler
    unsafe {
        asm!(
            "mov rsp, {stack}",
            "jmp {cont}",
            stack = in(reg) stack_top,
            cont = sym post_shard_return,
            options(noreturn),
        );
    }
}

/// Continuation point after a shard exits. Called by handle_sys_exit.
/// Execution resumes at supervisor_main after shard::run().
#[no_mangle]
extern "C" fn post_shard_return() {
    // When we get here, the shard has exited.
    // The main.rs code calls: shard::run(id); shard::destroy(id);
    // Since run() was called with noreturn/sysretq, we need to simulate
    // its return. We do this by calling destroy + the rest of supervisor_main.

    // Destroy shard 0
    destroy(0);

    crate::serial_println!();
    crate::serial_println!("coconutOS supervisor v0.2.0: shard lifecycle complete.");
    crate::serial_println!("Halting.");

    crate::halt();
}

/// Destroy a shard: zero all memory, free frames, tear down page tables.
pub fn destroy(id: usize) {
    let shard = unsafe { &mut (*(&raw mut SHARDS))[id] };
    assert!(
        shard.state == ShardState::Exited,
        "shard not in exited state"
    );

    // 1. Switch to supervisor page tables (should already be, but be safe)
    vmm::write_cr3(highhalf::supervisor_pml4());

    // 2. Zero and free all shard-allocated frames (except PML4 which we free last)
    // First zero the data frames (code + stack)
    for i in 1..shard.frame_count {
        let phys = shard.allocated_frames[i];
        if phys != 0 {
            // Zero the frame via HHDM
            let ptr = vmm::phys_to_virt(phys);
            unsafe {
                core::ptr::write_bytes(ptr, 0, 4096);
            }
            frame::free_frame(phys);
        }
    }

    // 3. Walk and free lower-half page table frames
    //    (PML4 entries 0-255 that we created for the shard)
    free_lower_half_tables(shard.pml4_phys);

    // 4. Zero and free the PML4 itself
    let pml4_ptr = vmm::phys_to_virt(shard.pml4_phys);
    unsafe {
        core::ptr::write_bytes(pml4_ptr, 0, 4096);
    }
    frame::free_frame(shard.pml4_phys);

    shard.state = ShardState::Destroyed;
    shard.pml4_phys = 0;
    shard.frame_count = 0;

    crate::serial_println!("Shard {}: destroyed (memory zeroed, frames freed)", id);
}

/// Walk and free all page table frames in the lower half (PML4 entries 0-255).
fn free_lower_half_tables(pml4_phys: u64) {
    let pml4 = vmm::phys_to_virt(pml4_phys) as *const u64;

    for i in 0..256 {
        let pml4e = unsafe { core::ptr::read_volatile(pml4.add(i)) };
        if pml4e & PTE_PRESENT == 0 {
            continue;
        }
        let pdpt_phys = pml4e & 0x000F_FFFF_FFFF_F000;
        let pdpt = vmm::phys_to_virt(pdpt_phys) as *const u64;

        for j in 0..512 {
            let pdpte = unsafe { core::ptr::read_volatile(pdpt.add(j)) };
            if pdpte & PTE_PRESENT == 0 {
                continue;
            }
            let pd_phys = pdpte & 0x000F_FFFF_FFFF_F000;
            let pd = vmm::phys_to_virt(pd_phys) as *const u64;

            for k in 0..512 {
                let pde = unsafe { core::ptr::read_volatile(pd.add(k)) };
                if pde & PTE_PRESENT == 0 {
                    continue;
                }
                // If it's a 2M page, skip (no PT level)
                if pde & 0x80 != 0 {
                    continue;
                }
                let pt_phys = pde & 0x000F_FFFF_FFFF_F000;
                // Zero and free PT
                unsafe {
                    core::ptr::write_bytes(vmm::phys_to_virt(pt_phys), 0, 4096);
                }
                frame::free_frame(pt_phys);
            }

            // Zero and free PD
            unsafe {
                core::ptr::write_bytes(vmm::phys_to_virt(pd_phys), 0, 4096);
            }
            frame::free_frame(pd_phys);
        }

        // Zero and free PDPT
        unsafe {
            core::ptr::write_bytes(vmm::phys_to_virt(pdpt_phys), 0, 4096);
        }
        frame::free_frame(pdpt_phys);
    }
}
