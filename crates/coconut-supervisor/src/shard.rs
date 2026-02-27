//! Shard lifecycle: create and destroy isolated user-mode address spaces.
//!
//! A shard is an isolated execution context with its own page tables (PML4),
//! user-mode code, and stack. The supervisor copies its upper-half PML4 entries
//! into the shard so syscalls/interrupts can access kernel code.

use crate::frame;
use crate::highhalf;
use crate::vmm::{self, PTE_NO_EXECUTE, PTE_PRESENT, PTE_USER, PTE_WRITABLE};

/// Maximum number of frames a single shard can allocate.
const MAX_SHARD_FRAMES: usize = 32;

/// Virtual address where the shard code is mapped.
pub const SHARD_CODE_VADDR: u64 = 0x1000;

/// Virtual address where the shard stack page is mapped (top of stack at 0x800000).
pub const SHARD_STACK_VADDR: u64 = 0x7FF000; // one 4K page, stack top = 0x800000

/// Initial stack pointer for the shard (top of stack page).
pub const SHARD_INITIAL_RSP: u64 = 0x800000;

pub const MAX_SHARDS: usize = 4;
pub const MAX_CAPS_PER_SHARD: usize = 16;

#[derive(Clone, Copy)]
pub struct CapEntry {
    pub valid: bool,
    pub cap_type: u8,
    pub resource_id: u32,
    pub rights: u16,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShardState {
    Free,
    Ready,
    Running,
    Blocked,
    Exited,
    Destroyed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Critical = 0,
    High = 1,
    Normal = 2,
    Low = 3,
}

pub const PRIORITY_LEVELS: usize = 4;

pub struct ShardDescriptor {
    pub state: ShardState,
    pub priority: Priority,
    pub pml4_phys: u64,
    pub allocated_frames: [u64; MAX_SHARD_FRAMES],
    pub frame_count: usize,
    pub exit_code: u64,
    /// Physical address of 4 KiB kernel stack frame for this shard.
    pub kernel_stack_phys: u64,
    /// HHDM virtual address of the top of the kernel stack.
    pub kernel_stack_top: u64,
    /// Saved RSP for context switching (points into kernel stack).
    pub saved_kernel_rsp: u64,
    /// Channel ID this shard is blocked on (usize::MAX = none).
    pub blocked_on_channel: usize,
    /// Capability table for this shard.
    pub caps: [CapEntry; MAX_CAPS_PER_SHARD],
    /// Allowed syscall categories (u64::MAX = unrestricted). Monotonically reduced via SYS_GPU_PLEDGE.
    pub gpu_pledge: u64,
    /// Unveiled VRAM offset within partition (only meaningful when vram_unveil_size > 0).
    pub vram_unveil_offset: u64,
    /// Unveiled VRAM size (0 = not yet unveiled, DMA unrestricted).
    pub vram_unveil_size: u64,
}

pub static mut SHARDS: [ShardDescriptor; MAX_SHARDS] = [const {
    ShardDescriptor {
        state: ShardState::Free,
        priority: Priority::Normal,
        pml4_phys: 0,
        allocated_frames: [0; MAX_SHARD_FRAMES],
        frame_count: 0,
        exit_code: 0,
        kernel_stack_phys: 0,
        kernel_stack_top: 0,
        saved_kernel_rsp: 0,
        blocked_on_channel: usize::MAX,
        caps: [CapEntry {
            valid: false,
            cap_type: 0,
            resource_id: 0,
            rights: 0,
        }; MAX_CAPS_PER_SHARD],
        gpu_pledge: u64::MAX,
        vram_unveil_offset: 0,
        vram_unveil_size: 0,
    }
}; MAX_SHARDS];

/// Currently running shard ID (or usize::MAX if none).
pub static mut CURRENT_SHARD: usize = usize::MAX;

// ---------------------------------------------------------------------------
// Embedded shard binary — filesystem demo shard
// ---------------------------------------------------------------------------

// fs_reader: Open /hello.txt, read contents, print via serial, close, exit.
core::arch::global_asm!(
    ".section .rodata",
    ".balign 16",
    ".global _fs_reader_start",
    ".global _fs_reader_end",
    "_fs_reader_start:",

    "mov rsp, 0x800000",

    // SYS_FS_OPEN("/hello.txt", 10) -> fd in rax
    "lea rdi, [rip + 1f]",          // path in code page
    "mov rsi, 10",                  // path_len = 10 ("/hello.txt")
    "mov rax, 30",                  // SYS_FS_OPEN
    "syscall",
    "mov r12, rax",                 // save fd (r12 preserved across syscalls)

    // SYS_FS_READ(fd, 0x7FFF00, 256) -> bytes_read in rax
    "mov rdi, r12",                 // fd
    "mov rsi, 0x7FFF00",            // buffer in stack page
    "mov rdx, 256",                 // max_len
    "mov rax, 31",                  // SYS_FS_READ
    "syscall",
    "mov r13, rax",                 // save bytes_read

    // SYS_SERIAL_WRITE(0x7FFF00, bytes_read) -> prints file contents
    "mov rdi, 0x7FFF00",            // buf
    "mov rsi, r13",                 // len = bytes_read
    "mov rax, 1",                   // SYS_SERIAL_WRITE
    "syscall",

    // SYS_FS_CLOSE(fd)
    "mov rdi, r12",                 // fd
    "mov rax, 33",                  // SYS_FS_CLOSE
    "syscall",

    // SYS_EXIT(0)
    "xor edi, edi",
    "mov rax, 0",
    "syscall",
    "2: hlt",
    "jmp 2b",

    // String data
    "1: .ascii \"/hello.txt\"",

    "_fs_reader_end:",
);

extern "C" {
    static _fs_reader_start: u8;
    static _fs_reader_end: u8;
}

/// Get the fs-reader shard binary (start, end) pointers.
pub fn fs_reader_binary() -> (*const u8, *const u8) {
    (
        (&raw const _fs_reader_start) as *const u8,
        (&raw const _fs_reader_end) as *const u8,
    )
}

/// Get the current shard ID.
pub fn current_shard() -> usize {
    unsafe { *(&raw const CURRENT_SHARD) }
}

/// Create a new shard with the given binary and priority.
/// Returns the shard ID (index).
pub fn create(
    binary_start: *const u8,
    binary_end: *const u8,
    name: &str,
    priority: Priority,
) -> usize {
    // Find a free slot
    let id = unsafe {
        let shards = &*(&raw const SHARDS);
        let mut found = usize::MAX;
        for i in 0..MAX_SHARDS {
            if shards[i].state == ShardState::Free {
                found = i;
                break;
            }
        }
        found
    };
    assert!(id != usize::MAX, "no free shard slots");

    crate::serial_println!("Shard {}: creating ({})...", id, name);

    let shard = unsafe { &mut (*(&raw mut SHARDS))[id] };

    // 1. Allocate PML4 for the shard
    let pml4_phys = frame::alloc_frame_zeroed().expect("shard: failed to alloc PML4");
    shard.pml4_phys = pml4_phys;
    shard.allocated_frames[0] = pml4_phys;
    shard.frame_count = 1;

    // 2. Copy supervisor's upper-half PML4 entries (256-511) into shard PML4
    let sup_pml4_phys = highhalf::supervisor_pml4();
    let sup_pml4 = vmm::phys_to_virt(sup_pml4_phys) as *const u64;
    let shard_pml4 = vmm::phys_to_virt(pml4_phys) as *mut u64;
    for i in 256..512 {
        unsafe {
            let entry = core::ptr::read_volatile(sup_pml4.add(i));
            core::ptr::write_volatile(shard_pml4.add(i), entry);
        }
    }

    // 3. Allocate code frame, copy shard binary, map at 0x1000 (R+X, USER)
    let code_phys = frame::alloc_frame_zeroed().expect("shard: failed to alloc code frame");
    shard.allocated_frames[shard.frame_count] = code_phys;
    shard.frame_count += 1;

    let code_size = binary_end as usize - binary_start as usize;
    let code_dest = vmm::phys_to_virt(code_phys);
    unsafe {
        core::ptr::copy_nonoverlapping(binary_start, code_dest, code_size);
    }

    // Map code page: PRESENT | USER (readable + executable, not writable, not NX)
    vmm::map_4k(pml4_phys, SHARD_CODE_VADDR, code_phys, PTE_USER);

    // 4. Allocate stack frame (zeroed), map at 0x7FF000 (R+W, USER, NX)
    let stack_phys = frame::alloc_frame_zeroed().expect("shard: failed to alloc stack frame");
    shard.allocated_frames[shard.frame_count] = stack_phys;
    shard.frame_count += 1;

    vmm::map_4k(
        pml4_phys,
        SHARD_STACK_VADDR,
        stack_phys,
        PTE_USER | PTE_WRITABLE | PTE_NO_EXECUTE,
    );

    // 5. Allocate per-shard kernel stack (4 KiB)
    let kstack_phys = frame::alloc_frame_zeroed().expect("shard: failed to alloc kernel stack");
    shard.allocated_frames[shard.frame_count] = kstack_phys;
    shard.frame_count += 1;
    shard.kernel_stack_phys = kstack_phys;
    shard.kernel_stack_top = vmm::phys_to_virt(kstack_phys) as u64 + 4096;

    // 6. Set up initial kernel stack for context_switch
    crate::scheduler::setup_initial_kernel_stack(id);

    shard.state = ShardState::Ready;
    shard.priority = priority;
    shard.blocked_on_channel = usize::MAX;

    crate::serial_println!(
        "Shard {}: code at virt {:#x} (R+X), stack at virt {:#x} (R+W+NX)",
        id,
        SHARD_CODE_VADDR,
        SHARD_STACK_VADDR
    );

    id
}

/// Handle SYS_EXIT from the current shard.
pub fn handle_sys_exit(exit_code: u64) {
    let id = current_shard();
    crate::serial_println!("Shard {}: sys_exit({})", id, exit_code);

    unsafe {
        let shard = &mut (*(&raw mut SHARDS))[id];
        shard.exit_code = exit_code;
        shard.state = ShardState::Exited;
        *(&raw mut CURRENT_SHARD) = usize::MAX;
    }

    // schedule_yield will context_switch back to the supervisor run_loop.
    // Since CURRENT_SHARD is usize::MAX, we use the exit path.
    crate::scheduler::schedule_yield_exit();
}

/// Destroy a shard: zero all memory, free frames, tear down page tables.
pub fn destroy(id: usize) {
    let shard = unsafe { &mut (*(&raw mut SHARDS))[id] };
    assert!(
        shard.state == ShardState::Exited,
        "shard not in exited state"
    );

    // 0. Clear all capabilities for this shard
    crate::capability::clear_shard(id);

    // 1. Switch to supervisor page tables (should already be, but be safe)
    vmm::write_cr3(highhalf::supervisor_pml4());

    // 2. Zero and free all shard-allocated frames (except PML4 which we free last)
    for i in 1..shard.frame_count {
        let phys = shard.allocated_frames[i];
        if phys != 0 {
            let ptr = vmm::phys_to_virt(phys);
            unsafe {
                core::ptr::write_bytes(ptr, 0, 4096);
            }
            frame::free_frame(phys);
        }
    }

    // 3. Walk and free lower-half page table frames
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
                if pde & 0x80 != 0 {
                    continue;
                }
                let pt_phys = pde & 0x000F_FFFF_FFFF_F000;
                unsafe {
                    core::ptr::write_bytes(vmm::phys_to_virt(pt_phys), 0, 4096);
                }
                frame::free_frame(pt_phys);
            }

            unsafe {
                core::ptr::write_bytes(vmm::phys_to_virt(pd_phys), 0, 4096);
            }
            frame::free_frame(pd_phys);
        }

        unsafe {
            core::ptr::write_bytes(vmm::phys_to_virt(pdpt_phys), 0, 4096);
        }
        frame::free_frame(pdpt_phys);
    }
}
