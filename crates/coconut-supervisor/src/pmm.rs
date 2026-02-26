//! Physical memory manager — bitmap allocator for 2 MiB regions.
//!
//! Each bit represents one 2 MiB region of physical memory.
//! Supports up to 1 TiB (512K regions = 8192 u64 entries).

use coconut_shared::{BootInfo, MemoryRegionDescriptor, MemoryRegionType};

/// Size of each allocatable region.
const REGION_SIZE: u64 = 2 * 1024 * 1024; // 2 MiB

/// Maximum number of regions (1 TiB / 2 MiB).
const MAX_REGIONS: usize = 512 * 1024;

/// Number of u64 words in the bitmap.
const BITMAP_WORDS: usize = MAX_REGIONS / 64;

/// Bitmap: 1 = free, 0 = allocated/reserved.
static mut BITMAP: [u64; BITMAP_WORDS] = [0u64; BITMAP_WORDS];

/// Total number of regions seen in the memory map.
static mut TOTAL_REGIONS: u64 = 0;
/// Number of usable (free) regions.
static mut USABLE_REGIONS: u64 = 0;
/// Number of reserved regions.
static mut _RESERVED_REGIONS: u64 = 0;

/// Mark a 2 MiB-aligned region as free.
fn mark_free(phys_addr: u64) {
    let index = (phys_addr / REGION_SIZE) as usize;
    if index < MAX_REGIONS {
        let word = index / 64;
        let bit = index % 64;
        unsafe {
            BITMAP[word] |= 1u64 << bit;
        }
    }
}

/// Mark a 2 MiB-aligned region as allocated.
fn mark_allocated(phys_addr: u64) {
    let index = (phys_addr / REGION_SIZE) as usize;
    if index < MAX_REGIONS {
        let word = index / 64;
        let bit = index % 64;
        unsafe {
            BITMAP[word] &= !(1u64 << bit);
        }
    }
}

/// Initialize the PMM from the boot info memory map.
pub fn init(boot_info: &BootInfo) {
    let memory_map = unsafe {
        core::slice::from_raw_parts(
            boot_info.memory_map_addr as *const MemoryRegionDescriptor,
            boot_info.memory_map_count as usize,
        )
    };

    let mut usable_bytes: u64 = 0;
    let mut reserved_bytes: u64 = 0;

    for region in memory_map {
        match region.region_type {
            MemoryRegionType::Usable | MemoryRegionType::BootloaderReclaimable => {
                // Mark each 2 MiB-aligned chunk within this region as free
                let start = align_up(region.phys_start, REGION_SIZE);
                let end = align_down(region.phys_start + region.size, REGION_SIZE);
                let mut addr = start;
                while addr < end {
                    mark_free(addr);
                    unsafe { USABLE_REGIONS += 1; }
                    addr += REGION_SIZE;
                }
                usable_bytes += region.size;
            }
            MemoryRegionType::SupervisorCode => {
                reserved_bytes += region.size;
            }
            // AcpiReclaimable and AcpiNvs are actual RAM, count as reserved
            MemoryRegionType::AcpiReclaimable | MemoryRegionType::AcpiNvs => {
                reserved_bytes += region.size;
            }
            // Skip MMIO and firmware Reserved regions — not usable RAM
            _ => {}
        }
    }

    // Make sure the supervisor region is marked as allocated
    let sup_start = align_down(boot_info.supervisor_phys_base, REGION_SIZE);
    let sup_end = align_up(boot_info.supervisor_phys_base + boot_info.supervisor_size, REGION_SIZE);
    let mut addr = sup_start;
    while addr < sup_end {
        mark_allocated(addr);
        addr += REGION_SIZE;
    }

    let total_bytes = usable_bytes + reserved_bytes;
    unsafe {
        TOTAL_REGIONS = total_bytes / REGION_SIZE;
    }

    // Print summary
    crate::serial_println!("Physical memory:");
    crate::serial_println!("  Total:      {} MiB", total_bytes / (1024 * 1024));
    let usable_count = count_free();
    crate::serial_println!(
        "  Usable:     {} MiB ({} regions @ 2 MiB)",
        usable_count * 2,
        usable_count
    );
    crate::serial_println!("  Reserved:   {} MiB", reserved_bytes / (1024 * 1024));
    crate::serial_println!(
        "  Supervisor: {} MiB",
        align_up(boot_info.supervisor_size, REGION_SIZE) / (1024 * 1024)
    );

    // Quick self-test: allocate and free a region
    if let Some(addr) = alloc_region() {
        crate::serial_println!("PMM self-test: allocated region at {:#x}", addr);
        free_region(addr);
        crate::serial_println!("PMM self-test: freed region at {:#x}", addr);
    }
}

/// Allocate a single 2 MiB region. Returns the physical address or None.
pub fn alloc_region() -> Option<u64> {
    unsafe {
        for word_idx in 0..BITMAP_WORDS {
            if BITMAP[word_idx] != 0 {
                let bit = BITMAP[word_idx].trailing_zeros() as usize;
                BITMAP[word_idx] &= !(1u64 << bit);
                let phys_addr = ((word_idx * 64) + bit) as u64 * REGION_SIZE;
                return Some(phys_addr);
            }
        }
    }
    None
}

/// Free a previously allocated 2 MiB region.
pub fn free_region(phys_addr: u64) {
    mark_free(phys_addr);
}

/// Count total free regions in the bitmap.
fn count_free() -> u64 {
    let mut count = 0u64;
    unsafe {
        for word_idx in 0..BITMAP_WORDS {
            count += BITMAP[word_idx].count_ones() as u64;
        }
    }
    count
}

const fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

const fn align_down(addr: u64, align: u64) -> u64 {
    addr & !(align - 1)
}
