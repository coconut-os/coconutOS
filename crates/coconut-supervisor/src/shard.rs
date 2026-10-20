//! Shard lifecycle: create and destroy isolated user-mode address spaces.
//!
//! A shard is an isolated execution context with its own page tables (PML4),
//! a user-mode ELF image, and stack. The supervisor copies its upper-half PML4 entries
//! into the shard so syscalls/interrupts can access kernel code.

use crate::elf;
use crate::frame;
use crate::highhalf;
use crate::vmm::{self, PTE_NO_EXECUTE, PTE_PRESENT, PTE_USER, PTE_WRITABLE};

/// Maximum number of frames a single shard can allocate.
/// 256 frames = 1 MiB — sufficient for inference shards with model + activations.
const MAX_SHARD_FRAMES: usize = 256;

/// Virtual address where the shard image starts (also its entry point).
pub const SHARD_CODE_VADDR: u64 = 0x1000;

/// Virtual address where the shard stack page is mapped (top of stack at 0x800000).
pub const SHARD_STACK_VADDR: u64 = 0x7FF000; // one 4K page, stack top = 0x800000

/// Initial stack pointer for the shard (top of stack page).
pub const SHARD_INITIAL_RSP: u64 = 0x800000;

pub const MAX_SHARDS: usize = 8;
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
    /// Virtual address past the last image page. [SHARD_CODE_VADDR, image_end) is fully mapped.
    pub image_end: u64,
    /// Start of the image's writable data+bss segment (0 = none).
    pub image_data_start: u64,
    /// End of the image's writable data+bss segment (exclusive).
    pub image_data_end: u64,
    /// Start of mmap'd data region (0 = no data region).
    pub data_start: u64,
    /// End of mmap'd data region (exclusive).
    pub data_end: u64,
    /// Shard name for profiling output (null-padded).
    pub name: [u8; 20],
    /// Total syscalls dispatched.
    pub syscall_count: u64,
    /// Total RDTSC cycles spent in syscall dispatch.
    pub syscall_cycles: u64,
    /// Number of times this shard was scheduled in.
    pub context_switches: u64,
    /// PIT tick when first scheduled (0 = never).
    pub first_scheduled_tick: u64,
    /// PIT tick when sys_exit called.
    pub last_exited_tick: u64,
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
        image_end: SHARD_CODE_VADDR,
        image_data_start: 0,
        image_data_end: 0,
        data_start: 0,
        data_end: 0,
        name: [0; 20],
        syscall_count: 0,
        syscall_cycles: 0,
        context_switches: 0,
        first_scheduled_tick: 0,
        last_exited_tick: 0,
    }
}; MAX_SHARDS];

/// Currently running shard ID (or usize::MAX if none).
pub static mut CURRENT_SHARD: usize = usize::MAX;

/// Get the current shard ID.
pub fn current_shard() -> usize {
    unsafe { *(&raw const CURRENT_SHARD) }
}

/// Create a new shard from an embedded ELF image with the given priority.
/// Returns the shard ID (index).
pub fn create(image: &[u8], name: &str, priority: Priority) -> usize {
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

    // Store shard name (truncated to 20 bytes, null-padded)
    let name_bytes = name.as_bytes();
    let copy_len = name_bytes.len().min(20);
    shard.name[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

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

    // 3. Map each PT_LOAD segment with the permissions the linker gave it.
    //    Segments must tile [SHARD_CODE_VADDR, image_end) without gaps:
    //    buffer validation accepts that whole range, so a hole would let a
    //    syscall reach an unmapped page from kernel mode.
    let elf = elf::parse(image);
    assert!(
        elf.entry == SHARD_CODE_VADDR,
        "shard: entry point must be SHARD_CODE_VADDR"
    );

    let mut image_end = SHARD_CODE_VADDR;
    for seg in &elf.segments[..elf.segment_count] {
        assert!(
            seg.vaddr == image_end,
            "shard: segments not contiguous and page-aligned"
        );
        assert!(
            !(seg.writable && seg.executable),
            "shard: segment is both W and X"
        );
        // vaddr == image_end <= SHARD_STACK_VADDR, so the subtraction cannot underflow
        assert!(
            seg.mem_size <= SHARD_STACK_VADDR - seg.vaddr,
            "shard: image overlaps stack"
        );
        let pages = seg.mem_size.div_ceil(4096);
        let seg_end = seg.vaddr + pages * 4096;

        let mut flags = PTE_USER;
        if seg.writable {
            assert!(
                shard.image_data_start == 0,
                "shard: more than one writable segment"
            );
            shard.image_data_start = seg.vaddr;
            shard.image_data_end = seg_end;
            flags |= PTE_WRITABLE;
        }
        if !seg.executable {
            flags |= PTE_NO_EXECUTE;
        }

        for page_idx in 0..pages as usize {
            let phys = frame::alloc_frame_zeroed().expect("shard: failed to alloc image frame");
            shard.allocated_frames[shard.frame_count] = phys;
            shard.frame_count += 1;

            // Copy the file-backed part of this page; the rest stays zero (.bss).
            let page_start = page_idx * 4096;
            if page_start < seg.file_size {
                let copy_len = (seg.file_size - page_start).min(4096);
                let src = &image[seg.file_offset + page_start..][..copy_len];
                // Sound: phys is a freshly allocated 4 KiB frame reachable via
                // the HHDM, and copy_len <= 4096.
                unsafe {
                    core::ptr::copy_nonoverlapping(src.as_ptr(), vmm::phys_to_virt(phys), copy_len);
                }
            }

            vmm::map_4k(pml4_phys, seg.vaddr + (page_idx as u64) * 4096, phys, flags);
        }

        crate::serial_println!(
            "Shard {}: segment {:#x}-{:#x} ({})",
            id,
            seg.vaddr,
            seg_end,
            match (seg.writable, seg.executable) {
                (false, true) => "R+X",
                (false, false) => "R+NX",
                _ => "R+W+NX",
            }
        );
        image_end = seg_end;
    }
    shard.image_end = image_end;

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
        "Shard {}: stack at virt {:#x} (R+W+NX)",
        id,
        SHARD_STACK_VADDR
    );

    id
}

/// Handle SYS_EXIT from the current shard.
pub fn handle_sys_exit(exit_code: u64) {
    let id = current_shard();
    crate::serial_println!("Shard {}: sys_exit({})", id, exit_code);
    crate::fs::release_shard(id);

    unsafe {
        let shard = &mut (*(&raw mut SHARDS))[id];
        shard.exit_code = exit_code;
        shard.last_exited_tick = *(&raw const crate::pit::TICKS);
        shard.state = ShardState::Exited;
        *(&raw mut CURRENT_SHARD) = usize::MAX;
    }

    // schedule_yield will context_switch back to the supervisor run_loop.
    // Since CURRENT_SHARD is usize::MAX, we use the exit path.
    crate::scheduler::schedule_yield_exit();
}

/// Handle SYS_MMAP: allocate data pages in the shard's address space.
///
/// Maps `num_pages` frames at [va_start, va_start + num_pages * 4096) with
/// USER | WRITABLE | NX. Valid range: [image_end, 0x7FF000).
/// Returns 0 on success, u64::MAX on error.
pub fn handle_sys_mmap(va_start: u64, num_pages: u64) -> u64 {
    let id = current_shard();
    let shard = unsafe { &mut (*(&raw mut SHARDS))[id] };

    // Alignment check
    if va_start & 0xFFF != 0 {
        crate::serial_println!("Shard {}: mmap: va_start {:#x} not page-aligned", id, va_start);
        return u64::MAX;
    }

    if num_pages == 0 || num_pages > 256 {
        return u64::MAX;
    }

    let size = num_pages * 4096;
    let va_end = va_start.checked_add(size).unwrap_or(u64::MAX);

    // Must be between image end and stack
    if va_start < shard.image_end || va_end > SHARD_STACK_VADDR {
        crate::serial_println!("Shard {}: mmap: range {:#x}-{:#x} out of bounds", id, va_start, va_end);
        return u64::MAX;
    }

    // New mappings must be contiguous with the existing data region.
    // Disjoint mappings would create unmapped gaps where buffer validation
    // passes but pages are absent, causing a kernel page fault on access.
    if shard.data_start != 0 {
        if va_start < shard.data_end && va_end > shard.data_start {
            crate::serial_println!("Shard {}: mmap: overlaps existing data region", id);
            return u64::MAX;
        }
        if va_end != shard.data_start && va_start != shard.data_end {
            crate::serial_println!("Shard {}: mmap: non-contiguous (must abut existing region)", id);
            return u64::MAX;
        }
    }

    // Check frame budget
    if shard.frame_count + num_pages as usize > MAX_SHARD_FRAMES {
        crate::serial_println!("Shard {}: mmap: would exceed frame budget", id);
        return u64::MAX;
    }

    // Allocate and map each page
    for i in 0..num_pages {
        let phys = match frame::alloc_frame_zeroed() {
            Some(p) => p,
            None => {
                crate::serial_println!("Shard {}: mmap: OOM at page {}", id, i);
                return u64::MAX;
            }
        };
        shard.allocated_frames[shard.frame_count] = phys;
        shard.frame_count += 1;

        vmm::map_4k(
            shard.pml4_phys,
            va_start + i * 4096,
            phys,
            PTE_USER | PTE_WRITABLE | PTE_NO_EXECUTE,
        );
    }

    // Extend or set data region tracking
    if shard.data_start == 0 {
        shard.data_start = va_start;
        shard.data_end = va_end;
    } else {
        // Extend region to cover the new mapping
        if va_start < shard.data_start {
            shard.data_start = va_start;
        }
        if va_end > shard.data_end {
            shard.data_end = va_end;
        }
    }

    crate::serial_println!(
        "Shard {}: mmap {:#x}-{:#x} ({} pages)",
        id, va_start, va_end, num_pages
    );

    0
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
