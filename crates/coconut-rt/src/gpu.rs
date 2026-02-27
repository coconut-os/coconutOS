//! GPU config page, VRAM allocator, command ring, and compute primitives.
//!
//! All VRAM access uses volatile reads/writes — device memory may be read
//! by GPU hardware asynchronously.

use core::ptr;

// ---------------------------------------------------------------------------
// Config page — kernel→shard partition parameters at VA 0x4000
// ---------------------------------------------------------------------------

const GPU_CONFIG_VADDR: usize = 0x4000;
const GPU_CONFIG_MAGIC: u32 = 0x4750_4346; // "GPCF"

/// GPU partition configuration, read from the kernel-provided config page.
pub struct GpuConfig {
    pub partition_id: u32,
    pub vram_size: u64,
    pub cu_count: u32,
    pub vram_vaddr: u64,
    pub mmio_vaddr: u64,
}

impl GpuConfig {
    /// Read and validate the config page. Returns `None` if magic is wrong.
    pub fn read() -> Option<Self> {
        let base = GPU_CONFIG_VADDR as *const u8;
        unsafe {
            let magic = ptr::read_volatile(base as *const u32);
            if magic != GPU_CONFIG_MAGIC {
                return None;
            }
            Some(GpuConfig {
                partition_id: ptr::read_volatile(base.add(4) as *const u32),
                vram_size: ptr::read_volatile(base.add(8) as *const u64),
                cu_count: ptr::read_volatile(base.add(0x10) as *const u32),
                vram_vaddr: ptr::read_volatile(base.add(0x18) as *const u64),
                mmio_vaddr: ptr::read_volatile(base.add(0x20) as *const u64),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// VRAM bump allocator — mirrors the ASM allocator layout
// ---------------------------------------------------------------------------

// Header at VRAM+0x00: magic(4), alloc_count(4), next_offset(4), total_size(4)
const ALLOC_MAGIC: u32 = 0x5641_4C4C; // "VALL"
// Table at VRAM+0x10: entries of 16 bytes each (type, offset, size, reserved)
const ALLOC_TABLE_OFFSET: usize = 0x10;
const ALLOC_ENTRY_SIZE: usize = 16;
const MAX_ALLOC_ENTRIES: u32 = 255;
const ALLOC_ALIGNMENT: u32 = 64; // GPU cache line

/// Bump allocator for VRAM with typed allocation entries.
pub struct VramAllocator {
    base: *mut u8,
}

impl VramAllocator {
    /// Initialize the allocator header at `base`. `total_size` is the partition's VRAM size.
    pub fn init(base: *mut u8, total_size: u32) -> Self {
        unsafe {
            ptr::write_volatile(base as *mut u32, ALLOC_MAGIC);
            ptr::write_volatile(base.add(4) as *mut u32, 0); // alloc_count
            ptr::write_volatile(base.add(8) as *mut u32, 0x1000); // skip allocator page
            ptr::write_volatile(base.add(12) as *mut u32, total_size);
        }
        VramAllocator { base }
    }

    /// Allocate `size` bytes with type tag. Returns VRAM-relative offset, or `None` on OOM.
    pub fn alloc(&mut self, alloc_type: u32, size: u32) -> Option<u32> {
        unsafe {
            let next = ptr::read_volatile(self.base.add(8) as *const u32);
            let aligned = (next + ALLOC_ALIGNMENT - 1) & !(ALLOC_ALIGNMENT - 1);
            let new_end = aligned.checked_add(size)?;
            let total = ptr::read_volatile(self.base.add(12) as *const u32);
            if new_end > total {
                return None;
            }

            let count = ptr::read_volatile(self.base.add(4) as *const u32);
            if count >= MAX_ALLOC_ENTRIES {
                return None;
            }

            // Write table entry
            let entry = self.base.add(ALLOC_TABLE_OFFSET + count as usize * ALLOC_ENTRY_SIZE);
            ptr::write_volatile(entry as *mut u32, alloc_type);
            ptr::write_volatile(entry.add(4) as *mut u32, aligned);
            ptr::write_volatile(entry.add(8) as *mut u32, size);
            ptr::write_volatile(entry.add(12) as *mut u32, 0);

            // Update header
            ptr::write_volatile(self.base.add(4) as *mut u32, count + 1);
            ptr::write_volatile(self.base.add(8) as *mut u32, new_end);

            Some(aligned)
        }
    }

    /// Free a VRAM allocation: zero the region and mark the entry free.
    /// Returns `false` if the offset wasn't found in the allocation table.
    pub fn free(&mut self, offset: u32) -> bool {
        unsafe {
            let count = ptr::read_volatile(self.base.add(4) as *const u32);
            for i in 0..count {
                let entry =
                    self.base.add(ALLOC_TABLE_OFFSET + i as usize * ALLOC_ENTRY_SIZE);
                let entry_offset = ptr::read_volatile(entry.add(4) as *const u32);
                if entry_offset != offset {
                    continue;
                }

                let size = ptr::read_volatile(entry.add(8) as *const u32);
                // Mark freed
                ptr::write_volatile(entry as *mut u32, 0);

                // Zero the region (qword at a time for efficiency)
                let dest = self.base.add(offset as usize);
                let qwords = size as usize / 8;
                for q in 0..qwords {
                    ptr::write_volatile(dest.add(q * 8) as *mut u64, 0);
                }

                return true;
            }
        }
        false
    }

    /// Read the current allocation count from the header.
    pub fn alloc_count(&self) -> u32 {
        unsafe { ptr::read_volatile(self.base.add(4) as *const u32) }
    }

    /// Zero the entire allocator page (first 4 KiB of VRAM).
    pub fn zero_page(&self) {
        unsafe {
            let qwords = 4096 / 8;
            for q in 0..qwords {
                ptr::write_volatile(self.base.add(q * 8) as *mut u64, 0);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Command ring — VRAM-based dispatch queue
// ---------------------------------------------------------------------------

const RING_MAGIC: u32 = 0x474E_4952; // "RING"
const RING_ENTRY_SIZE: u32 = 32;

/// VRAM command ring for compute dispatch.
pub struct CommandRing {
    base: *mut u8,
}

impl CommandRing {
    /// Initialize a command ring at `base` (VRAM pointer to allocated region).
    pub fn init(base: *mut u8) -> Self {
        unsafe {
            ptr::write_volatile(base as *mut u32, RING_MAGIC);
            ptr::write_volatile(base.add(4) as *mut u32, 0); // write_ptr
            ptr::write_volatile(base.add(8) as *mut u32, 0); // read_ptr
            ptr::write_volatile(base.add(12) as *mut u32, 0x1000); // ring_size
        }
        CommandRing { base }
    }

    /// Verify ring header magic. Returns false on mismatch.
    pub fn verify(&self) -> bool {
        unsafe { ptr::read_volatile(self.base as *const u32) == RING_MAGIC }
    }

    /// Submit a matmul dispatch command.
    ///
    /// `a_off`, `b_off`, `c_off` are VRAM-relative offsets for matrices A, B, C.
    pub fn submit_matmul(&mut self, a_off: u32, b_off: u32, c_off: u32, dim: u32) {
        unsafe {
            let entry = self.base.add(0x10); // first entry after header
            ptr::write_volatile(entry as *mut u32, 1); // DISPATCH_COMPUTE
            ptr::write_volatile(entry.add(4) as *mut u32, 0); // PENDING
            ptr::write_volatile(entry.add(8) as *mut u32, a_off);
            ptr::write_volatile(entry.add(12) as *mut u32, b_off);
            ptr::write_volatile(entry.add(16) as *mut u32, c_off);
            ptr::write_volatile(entry.add(20) as *mut u32, dim);
            ptr::write_volatile(entry.add(24) as *mut u32, 1); // fence_value
            ptr::write_volatile(entry.add(28) as *mut u32, 0); // reserved
            // Advance write_ptr past one entry
            ptr::write_volatile(self.base.add(4) as *mut u32, RING_ENTRY_SIZE);
        }
    }

    /// Read a submitted command's matrix offsets and dimension.
    ///
    /// Returns `(a_off, b_off, c_off, dim)` from the first ring entry.
    pub fn read_command(&self) -> (u32, u32, u32, u32) {
        unsafe {
            let entry = self.base.add(0x10);
            (
                ptr::read_volatile(entry.add(8) as *const u32),
                ptr::read_volatile(entry.add(12) as *const u32),
                ptr::read_volatile(entry.add(16) as *const u32),
                ptr::read_volatile(entry.add(20) as *const u32),
            )
        }
    }

    /// Mark the first command complete and advance read_ptr.
    pub fn complete(&mut self) {
        unsafe {
            let entry = self.base.add(0x10);
            ptr::write_volatile(entry.add(4) as *mut u32, 1); // COMPLETE
            ptr::write_volatile(self.base.add(8) as *mut u32, RING_ENTRY_SIZE);
        }
    }
}

// ---------------------------------------------------------------------------
// Matmul — 4x4 integer matrix multiply
// ---------------------------------------------------------------------------

/// Compute C = A × B for 4×4 u32 matrices in VRAM.
///
/// All pointers must be valid VRAM-mapped addresses. Uses volatile access
/// because the memory is device-mapped (uncacheable PCD+PWT).
pub fn matmul_4x4(a: *const u32, b: *const u32, c: *mut u32) {
    for i in 0..4u32 {
        for j in 0..4u32 {
            let mut acc: u32 = 0;
            for k in 0..4u32 {
                unsafe {
                    let a_val = ptr::read_volatile(a.add((i * 4 + k) as usize));
                    let b_val = ptr::read_volatile(b.add((k * 4 + j) as usize));
                    acc = acc.wrapping_add(a_val.wrapping_mul(b_val));
                }
            }
            unsafe {
                ptr::write_volatile(c.add((i * 4 + j) as usize), acc);
            }
        }
    }
}
