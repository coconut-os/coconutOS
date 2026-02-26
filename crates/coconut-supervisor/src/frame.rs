//! 4 KiB frame allocator — sub-allocates 2 MiB regions from the PMM.
//!
//! Each `SubRegion` wraps a 2 MiB chunk as a bitmap of 512 × 4 KiB frames.
//! When all frames in a sub-region are exhausted, a new 2 MiB region is
//! requested from the PMM.

use crate::pmm;
use crate::vmm;

const FRAME_SIZE: u64 = 4096;
const FRAMES_PER_REGION: usize = 512; // 2 MiB / 4 KiB
const BITMAP_WORDS: usize = FRAMES_PER_REGION / 64; // 8 u64s per sub-region
const MAX_SUB_REGIONS: usize = 64;

/// Bitmap tracking 512 × 4 KiB frames within a single 2 MiB region.
struct SubRegion {
    /// Physical base address of the 2 MiB region.
    base_phys: u64,
    /// Bitmap: 1 = free, 0 = allocated.
    bitmap: [u64; BITMAP_WORDS],
}

impl SubRegion {
    /// Create a new sub-region with all frames free.
    fn new(base_phys: u64) -> Self {
        Self {
            base_phys,
            bitmap: [!0u64; BITMAP_WORDS], // all bits set = all free
        }
    }

    /// Allocate a single 4 KiB frame. Returns physical address or None.
    fn alloc(&mut self) -> Option<u64> {
        for word_idx in 0..BITMAP_WORDS {
            if self.bitmap[word_idx] != 0 {
                let bit = self.bitmap[word_idx].trailing_zeros() as usize;
                self.bitmap[word_idx] &= !(1u64 << bit);
                let offset = ((word_idx * 64) + bit) as u64 * FRAME_SIZE;
                return Some(self.base_phys + offset);
            }
        }
        None
    }

    /// Free a 4 KiB frame by physical address. Returns true if this sub-region owns it.
    fn free(&mut self, phys: u64) -> bool {
        if phys < self.base_phys || phys >= self.base_phys + (FRAMES_PER_REGION as u64 * FRAME_SIZE) {
            return false;
        }
        let frame_idx = ((phys - self.base_phys) / FRAME_SIZE) as usize;
        let word = frame_idx / 64;
        let bit = frame_idx % 64;
        self.bitmap[word] |= 1u64 << bit;
        true
    }

    /// Check if this sub-region contains the given physical address.
    fn contains(&self, phys: u64) -> bool {
        phys >= self.base_phys && phys < self.base_phys + (FRAMES_PER_REGION as u64 * FRAME_SIZE)
    }
}

static mut SUB_REGIONS: [Option<SubRegion>; MAX_SUB_REGIONS] = [const { None }; MAX_SUB_REGIONS];
static mut REGION_COUNT: usize = 0;

/// Initialize the frame allocator (no-op; sub-regions are consumed lazily).
pub fn init() {
    crate::serial_println!("Frame allocator: ready");
}

/// Allocate a single 4 KiB frame. Returns physical address or None.
pub fn alloc_frame() -> Option<u64> {
    unsafe {
        // Try existing sub-regions first
        for i in 0..REGION_COUNT {
            if let Some(ref mut sr) = (*(&raw mut SUB_REGIONS))[i] {
                if let Some(phys) = sr.alloc() {
                    return Some(phys);
                }
            }
        }

        // Need a new 2 MiB region from PMM
        if REGION_COUNT >= MAX_SUB_REGIONS {
            return None;
        }
        let region_phys = pmm::alloc_region()?;
        let idx = REGION_COUNT;
        REGION_COUNT += 1;
        (*(&raw mut SUB_REGIONS))[idx] = Some(SubRegion::new(region_phys));
        if let Some(ref mut sr) = (*(&raw mut SUB_REGIONS))[idx] {
            sr.alloc()
        } else {
            None
        }
    }
}

/// Allocate a 4 KiB frame and zero it. Returns physical address or None.
pub fn alloc_frame_zeroed() -> Option<u64> {
    let phys = alloc_frame()?;
    let ptr = vmm::phys_to_virt(phys);
    unsafe {
        core::ptr::write_bytes(ptr, 0, FRAME_SIZE as usize);
    }
    Some(phys)
}

/// Free a previously allocated 4 KiB frame.
pub fn free_frame(phys: u64) {
    unsafe {
        for i in 0..REGION_COUNT {
            if let Some(ref mut sr) = (*(&raw mut SUB_REGIONS))[i] {
                if sr.free(phys) {
                    return;
                }
            }
        }
    }
}
