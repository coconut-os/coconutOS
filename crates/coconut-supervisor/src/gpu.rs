//! GPU HAL shard — device selection, BAR mapping, GPU partitioning.
//!
//! Partitions the GPU's VRAM and virtual CUs for multi-shard isolation.
//! Creates a ring-3 HAL shard per partition with its own VRAM slice.
//! Dispatches compute commands through a VRAM-based command ring. Uses QEMU's
//! standard VGA (1234:1111) with CPU-simulated compute as the dispatch backend.

use crate::capability;
use crate::channel;
use crate::frame;
use crate::pci;
use crate::shard::{self, Priority};
use crate::vmm::{
    self, PTE_CACHE_DISABLE, PTE_NO_EXECUTE, PTE_USER, PTE_WRITABLE, PTE_WRITE_THROUGH,
};

/// Maximum VRAM bytes to map (limits page table frame consumption).
const MAX_VRAM_MAP: u64 = 32 * 1024 * 1024;

/// ASLR region: [0x800000, 0x3F000000). Above stack, within PML4[0]/PDPT[0].
const ASLR_START: u64 = 0x80_0000;
const ASLR_END: u64 = 0x3F00_0000;

// ---------------------------------------------------------------------------
// PRNG — xorshift64 seeded from RDTSC for ASLR entropy
// ---------------------------------------------------------------------------

/// Single-core, seeded once in init() before shard creation.
static mut PRNG_STATE: u64 = 0;

fn prng_seed() {
    let tsc: u64;
    // Sound: rdtsc is always available on x86-64, no memory side-effects
    unsafe {
        core::arch::asm!(
            "rdtsc", "shl rdx, 32", "or rax, rdx",
            out("rax") tsc, out("rdx") _,
            options(nomem, nostack),
        );
    }
    // Ensure non-zero state (xorshift requirement)
    unsafe { *(&raw mut PRNG_STATE) = tsc | 1; }
}

fn prng_next() -> u64 {
    let mut s = unsafe { *(&raw const PRNG_STATE) };
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    unsafe { *(&raw mut PRNG_STATE) = s; }
    s
}

/// Generate a random page-aligned virtual address for a mapping of `map_size` bytes.
fn random_vaddr(map_size: u64) -> u64 {
    let range = ASLR_END - ASLR_START - map_size;
    let pages = range / 4096;
    let offset = (prng_next() % pages) * 4096;
    ASLR_START + offset
}

const MAX_GPU_PARTITIONS: usize = 2;
const TOTAL_VIRTUAL_CUS: u32 = 8;

/// User VA for the kernel→shard config page (between code 0x1000 and stack 0x7FF000).
const GPU_CONFIG_VADDR: u64 = 0x4000;
const GPU_CONFIG_MAGIC: u32 = 0x4750_4346; // "GPCF"

struct GpuPartition {
    vram_offset: u64,
    vram_size: u64,
    cu_count: u32,
    id: u32,
    /// Shard that owns this partition (usize::MAX = unassigned).
    owner_shard: usize,
}

/// Kernel-mapped VRAM base pointer. Set during init() via vmm::map_mmio() so
/// the DMA handler can copy between partitions without a user mapping.
static mut VRAM_KERN_PTR: *mut u8 = core::ptr::null_mut();
/// Physical base address of the VRAM BAR, saved for offset calculations.
static mut VRAM_PHYS_BASE: u64 = 0;
/// Total VRAM BAR size in bytes.
static mut VRAM_TOTAL_SIZE: u64 = 0;

// Single-core, no contention. Accessed only from gpu::init().
static mut PARTITIONS: [GpuPartition; MAX_GPU_PARTITIONS] = [const {
    GpuPartition {
        vram_offset: 0,
        vram_size: 0,
        cu_count: 0,
        id: 0,
        owner_shard: usize::MAX,
    }
}; MAX_GPU_PARTITIONS];
static mut PARTITION_COUNT: usize = 0;

// ---------------------------------------------------------------------------
// pledge / unveil handlers
// ---------------------------------------------------------------------------

/// Handle SYS_GPU_PLEDGE: monotonically restrict allowed syscall categories.
///
/// Performs `shard.gpu_pledge &= mask` — can only remove permissions, never add.
pub fn handle_gpu_pledge(mask: u64) -> u64 {
    let id = shard::current_shard();
    let shard = unsafe { &mut (*(&raw mut shard::SHARDS))[id] };
    shard.gpu_pledge &= mask;
    crate::serial_println!("Shard {}: GPU pledge {:#x}", id, shard.gpu_pledge);
    0
}

/// Handle SYS_GPU_UNVEIL: lock the VRAM range this shard may access via DMA.
///
/// One-shot: subsequent calls return u64::MAX. Validates offset+size fits
/// within the shard's partition VRAM.
pub fn handle_gpu_unveil(offset: u64, size: u64) -> u64 {
    let id = shard::current_shard();
    let shard = unsafe { &mut (*(&raw mut shard::SHARDS))[id] };

    // One-shot: already unveiled
    if shard.vram_unveil_size != 0 {
        return u64::MAX;
    }

    if size == 0 {
        return u64::MAX;
    }

    // Find caller's partition to validate range
    let count = unsafe { *(&raw const PARTITION_COUNT) };
    let mut part_vram_size = 0u64;
    for i in 0..count {
        let p = unsafe { &(*(&raw const PARTITIONS))[i] };
        if p.owner_shard == id {
            part_vram_size = p.vram_size;
            break;
        }
    }

    if part_vram_size == 0 {
        return u64::MAX;
    }

    // Validate range fits within partition
    if offset.checked_add(size).is_none() || offset + size > part_vram_size {
        return u64::MAX;
    }

    shard.vram_unveil_offset = offset;
    shard.vram_unveil_size = size;

    crate::serial_println!(
        "Shard {}: VRAM unveiled [{:#x}, {:#x})",
        id,
        offset,
        offset + size
    );
    0
}

/// Check whether a DMA range [offset, offset+len) falls within a shard's unveiled VRAM.
/// Returns true if the shard has not unveiled (unrestricted) or the range is within bounds.
fn unveil_allows(shard_id: usize, offset: u64, len: u64) -> bool {
    let shard = unsafe { &(*(&raw const shard::SHARDS))[shard_id] };

    // Not unveiled → unrestricted
    if shard.vram_unveil_size == 0 {
        return true;
    }

    let end = match offset.checked_add(len) {
        Some(e) => e,
        None => return false,
    };

    offset >= shard.vram_unveil_offset && end <= shard.vram_unveil_offset + shard.vram_unveil_size
}

// ---------------------------------------------------------------------------
// DMA handler — kernel-mediated VRAM copy between partitions
// ---------------------------------------------------------------------------

/// Handle SYS_GPU_DMA: copy data between VRAM partitions.
///
/// a0 = target partition ID, a1 = source VRAM offset (within caller's partition),
/// a2 = packed (dst_vram_offset << 32 | len).
/// Returns 0 on success, u64::MAX on error.
pub fn handle_dma(target_part: u64, src_offset: u64, packed_dst_len: u64) -> u64 {
    let caller = shard::current_shard();
    let dst_offset = packed_dst_len >> 32;
    let len = packed_dst_len & 0xFFFF_FFFF;

    if len == 0 {
        return u64::MAX;
    }

    let target_id = target_part as usize;
    let count = unsafe { *(&raw const PARTITION_COUNT) };
    if target_id >= count {
        return u64::MAX;
    }

    // Find caller's partition
    let mut src_part_idx = usize::MAX;
    for i in 0..count {
        let p = unsafe { &(*(&raw const PARTITIONS))[i] };
        if p.owner_shard == caller {
            src_part_idx = i;
            break;
        }
    }
    if src_part_idx == usize::MAX || src_part_idx == target_id {
        return u64::MAX;
    }

    // Check CAP_GPU_DMA for the target partition
    if !capability::check(
        caller,
        coconut_shared::CAP_GPU_DMA,
        target_id as u32,
        coconut_shared::RIGHT_GPU_DMA_WRITE,
    ) {
        crate::serial_println!("Shard {}: DMA capability denied for partition {}", caller, target_id);
        return u64::MAX;
    }

    let src_part = unsafe { &(*(&raw const PARTITIONS))[src_part_idx] };
    let dst_part = unsafe { &(*(&raw const PARTITIONS))[target_id] };

    // Bounds-check within each partition's VRAM slice
    if src_offset.checked_add(len).is_none() || src_offset + len > src_part.vram_size {
        return u64::MAX;
    }
    if dst_offset.checked_add(len).is_none() || dst_offset + len > dst_part.vram_size {
        return u64::MAX;
    }

    // Enforce unveil on source shard (caller)
    if !unveil_allows(caller, src_offset, len) {
        crate::serial_println!("Shard {}: DMA source outside unveiled range", caller);
        return u64::MAX;
    }

    // Enforce unveil on destination shard
    let dst_shard = dst_part.owner_shard;
    if dst_shard != usize::MAX && !unveil_allows(dst_shard, dst_offset, len) {
        crate::serial_println!("Shard {}: DMA dest outside target's unveiled range", caller);
        return u64::MAX;
    }

    let kern_ptr = unsafe { *(&raw const VRAM_KERN_PTR) };
    if kern_ptr.is_null() {
        return u64::MAX;
    }

    // Copy via kernel VRAM mapping: src partition's absolute offset → dst partition's absolute offset
    let src_abs = src_part.vram_offset + src_offset;
    let dst_abs = dst_part.vram_offset + dst_offset;

    unsafe {
        core::ptr::copy_nonoverlapping(
            kern_ptr.add(src_abs as usize),
            kern_ptr.add(dst_abs as usize),
            len as usize,
        );
    }

    crate::serial_println!(
        "GPU DMA: {} bytes, partition {} offset {:#x} -> partition {} offset {:#x}",
        len,
        src_part_idx,
        src_offset,
        target_id,
        dst_offset
    );

    0
}

// ---------------------------------------------------------------------------
// Embedded GPU HAL shard binary (built from coconut-shard-gpu crate)
// ---------------------------------------------------------------------------

/// Flat binary produced by llvm-objcopy from the coconut-shard-gpu ELF.
/// Embedded at compile time; loaded into shard code pages at VA 0x1000.
static GPU_SHARD_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shard-gpu.bin"));

fn hal_binary() -> (*const u8, *const u8) {
    let start = GPU_SHARD_BIN.as_ptr();
    // Sound: pointer arithmetic within the bounds of the static slice
    let end = unsafe { start.add(GPU_SHARD_BIN.len()) };
    (start, end)
}

// ---------------------------------------------------------------------------
// GPU partitioning
// ---------------------------------------------------------------------------

/// Split VRAM evenly into MAX_GPU_PARTITIONS partitions, dividing CUs equally.
fn create_partitions(vram_size: u64) {
    let count = MAX_GPU_PARTITIONS;
    let per_part = vram_size / count as u64;
    let cus_per = TOTAL_VIRTUAL_CUS / count as u32;

    for i in 0..count {
        unsafe {
            let p = &mut (*(&raw mut PARTITIONS))[i];
            p.vram_offset = i as u64 * per_part;
            p.vram_size = per_part;
            p.cu_count = cus_per;
            p.id = i as u32;
            p.owner_shard = usize::MAX;
        }
    }

    unsafe {
        *(&raw mut PARTITION_COUNT) = count;
    }

    crate::serial_println!(
        "GPU: {} partitions ({} MiB VRAM each, {} CUs each)",
        count,
        per_part / (1024 * 1024),
        cus_per
    );
}

/// Create a config page for a partition and map it into the shard at GPU_CONFIG_VADDR.
///
/// The config page is a single RAM frame with partition parameters that the
/// HAL shard reads at startup. Mapped read-only (no PTE_WRITABLE) with NX.
/// Tracked in shard.allocated_frames for cleanup on destroy.
fn create_config_page(
    shard_id: usize,
    pml4_phys: u64,
    partition: &GpuPartition,
    vram_vaddr: u64,
    mmio_vaddr: u64,
) {
    let config_phys = frame::alloc_frame_zeroed().expect("GPU config frame");

    // Write partition parameters via HHDM
    let base = vmm::phys_to_virt(config_phys);
    unsafe {
        core::ptr::write_volatile(base as *mut u32, GPU_CONFIG_MAGIC);
        core::ptr::write_volatile(base.add(4) as *mut u32, partition.id);
        core::ptr::write_volatile(base.add(8) as *mut u64, partition.vram_size);
        core::ptr::write_volatile(base.add(0x10) as *mut u32, partition.cu_count);
        // ASLR: randomized BAR virtual addresses for this shard
        core::ptr::write_volatile(base.add(0x18) as *mut u64, vram_vaddr);
        core::ptr::write_volatile(base.add(0x20) as *mut u64, mmio_vaddr);
    }

    // Map as user-readable, not writable, not executable
    vmm::map_4k(pml4_phys, GPU_CONFIG_VADDR, config_phys, PTE_USER | PTE_NO_EXECUTE);

    // Track frame for cleanup on shard destroy
    let shard = unsafe { &mut (*(&raw mut shard::SHARDS))[shard_id] };
    let fc = shard.frame_count;
    assert!(fc < shard.allocated_frames.len(), "shard frame table full");
    shard.allocated_frames[fc] = config_phys;
    shard.frame_count = fc + 1;

    crate::serial_println!(
        "GPU: config page at virt {:#x} (partition {}, {} MiB VRAM, {} CUs)",
        GPU_CONFIG_VADDR,
        partition.id,
        partition.vram_size / (1024 * 1024),
        partition.cu_count
    );
}

/// Map a partition's VRAM slice into a shard's address space.
///
/// Only maps the partition's portion of the VRAM BAR, starting at
/// vram_bar.phys_base + partition.vram_offset for partition.vram_size bytes.
/// The shard sees VRAM at the ASLR-randomized `vram_vaddr`.
fn map_vram_partition(
    pml4_phys: u64,
    vram_bar: &pci::BarInfo,
    partition: &GpuPartition,
    vram_vaddr: u64,
) {
    let phys_start = vram_bar.phys_base + partition.vram_offset;
    let map_size = partition.vram_size.min(MAX_VRAM_MAP);
    let flags = PTE_USER | PTE_WRITABLE | PTE_NO_EXECUTE | PTE_CACHE_DISABLE | PTE_WRITE_THROUGH;

    let mut offset = 0u64;
    while offset < map_size {
        vmm::map_4k(pml4_phys, vram_vaddr + offset, phys_start + offset, flags);
        offset += 4096;
    }

    crate::serial_println!(
        "GPU: mapped VRAM partition {} phys {:#x}+{:#x} -> shard virt {:#x} ({} pages)",
        partition.id,
        vram_bar.phys_base,
        partition.vram_offset,
        vram_vaddr,
        map_size / 4096
    );
}

// ---------------------------------------------------------------------------
// BAR mapping
// ---------------------------------------------------------------------------

/// Map a PCI BAR region into a shard's address space at the given virtual base.
///
/// Pages are mapped uncacheable (PCD+PWT) with user access and NX. Device BAR
/// pages are NOT tracked in shard.allocated_frames — they're device memory, not
/// RAM, so the frame allocator must not reclaim them.
fn map_bar_to_shard(pml4_phys: u64, bar: &pci::BarInfo, virt_base: u64, cap: u64, label: &str) {
    let map_size = bar.size.min(cap);
    let flags = PTE_USER | PTE_WRITABLE | PTE_NO_EXECUTE | PTE_CACHE_DISABLE | PTE_WRITE_THROUGH;

    let mut offset = 0u64;
    while offset < map_size {
        vmm::map_4k(pml4_phys, virt_base + offset, bar.phys_base + offset, flags);
        offset += 4096;
    }

    crate::serial_println!(
        "GPU: mapped {} BAR phys {:#x} -> shard virt {:#x} ({} pages)",
        label,
        bar.phys_base,
        virt_base,
        map_size / 4096
    );
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize the GPU subsystem: find GPU device, create HAL shard, map BARs.
///
/// Does nothing if no display controller is found on the PCI bus.
pub fn init() {
    let dev = match pci::find_display_device() {
        Some(d) => d,
        None => {
            crate::serial_println!("GPU: no display device found, skipping");
            return;
        }
    };

    crate::serial_println!(
        "GPU: found {:04x}:{:04x} at {:02x}:{:02x}.{}",
        dev.vendor_id,
        dev.device_id,
        dev.bus,
        dev.device,
        dev.function
    );

    let bars = pci::probe_bars(&dev);

    for (i, bar) in bars.iter().enumerate() {
        if bar.size == 0 {
            continue;
        }
        let pf = if bar.prefetchable { "prefetchable " } else { "" };
        let bits = if bar.is_64bit { "64-bit" } else { "32-bit" };
        crate::serial_println!(
            "GPU: BAR{} phys {:#x} size {} KiB {}{}",
            i,
            bar.phys_base,
            bar.size / 1024,
            pf,
            bits
        );
    }

    // Find MMIO BAR (first non-prefetchable memory BAR) and VRAM BAR (first prefetchable)
    let mut mmio_bar = None;
    let mut vram_bar = None;
    for bar in &bars {
        if bar.size == 0 || !bar.is_memory {
            continue;
        }
        if bar.prefetchable && vram_bar.is_none() {
            vram_bar = Some(*bar);
        } else if !bar.prefetchable && mmio_bar.is_none() {
            mmio_bar = Some(*bar);
        }
    }

    let mmio = match mmio_bar {
        Some(b) => b,
        None => {
            crate::serial_println!("GPU: no MMIO BAR found, skipping HAL shard");
            return;
        }
    };
    let vram = match vram_bar {
        Some(b) => b,
        None => {
            crate::serial_println!("GPU: no VRAM BAR found, skipping HAL shard");
            return;
        }
    };

    // Map VRAM BAR into kernel virtual space for DMA handler access.
    // VRAM is above the 1 GiB HHDM range, so we need an explicit MMIO mapping.
    let vram_kern = vmm::map_mmio(vram.phys_base, vram.size);
    unsafe {
        *(&raw mut VRAM_KERN_PTR) = vram_kern;
        *(&raw mut VRAM_PHYS_BASE) = vram.phys_base;
        *(&raw mut VRAM_TOTAL_SIZE) = vram.size;
    }
    crate::serial_println!(
        "GPU: kernel VRAM mapping at {:p} ({} MiB)",
        vram_kern,
        vram.size / (1024 * 1024)
    );

    // Partition VRAM and virtual CUs
    create_partitions(vram.size);

    // Seed PRNG from TSC before creating shards — provides per-boot entropy
    prng_seed();

    // Create one HAL shard per partition
    let (start, end) = hal_binary();
    let count = unsafe { *(&raw const PARTITION_COUNT) };
    // Track shard IDs for capability setup
    let mut shard_ids = [usize::MAX; MAX_GPU_PARTITIONS];

    for pi in 0..count {
        let partition = unsafe { &(*(&raw const PARTITIONS))[pi] };
        let vram_map_size = partition.vram_size.min(MAX_VRAM_MAP);

        // ASLR: randomize VRAM and MMIO virtual addresses per shard
        let vram_vaddr = random_vaddr(vram_map_size);
        let mut mmio_vaddr = random_vaddr(mmio.size);
        // Retry if MMIO overlaps VRAM range (vanishingly unlikely in ~1 GiB space)
        while mmio_vaddr < vram_vaddr + vram_map_size && vram_vaddr < mmio_vaddr + mmio.size {
            mmio_vaddr = random_vaddr(mmio.size);
        }

        crate::serial_println!(
            "GPU: ASLR shard {}: VRAM {:#x}, MMIO {:#x}",
            pi,
            vram_vaddr,
            mmio_vaddr
        );

        let id = shard::create(start, end, "gpu-hal", Priority::High);
        let pml4_phys = unsafe { (*(&raw const shard::SHARDS))[id].pml4_phys };

        create_config_page(id, pml4_phys, partition, vram_vaddr, mmio_vaddr);
        map_bar_to_shard(pml4_phys, &mmio, mmio_vaddr, mmio.size, "MMIO");
        map_vram_partition(pml4_phys, &vram, partition, vram_vaddr);

        unsafe {
            (*(&raw mut PARTITIONS))[pi].owner_shard = id;
        }
        shard_ids[pi] = id;
        crate::serial_println!("GPU: HAL shard {} created (partition {})", id, pi);
    }

    // Set up DMA channel and capabilities between partition 0 and partition 1
    if count >= 2 {
        let s0 = shard_ids[0];
        let s1 = shard_ids[1];

        // Channel 0: shard 0 <-> shard 1
        channel::init(0, s0, s1);
        crate::serial_println!("GPU: DMA channel 0 (shard {} <-> shard {})", s0, s1);

        // Shard 0: SEND on channel 0
        capability::grant_to_shard(s0, coconut_shared::CAP_CHANNEL, 0, coconut_shared::RIGHT_CHANNEL_SEND);
        // Shard 1: RECV on channel 0
        capability::grant_to_shard(s1, coconut_shared::CAP_CHANNEL, 0, coconut_shared::RIGHT_CHANNEL_RECV);
        // Shard 0: DMA write to partition 1
        capability::grant_to_shard(s0, coconut_shared::CAP_GPU_DMA, 1, coconut_shared::RIGHT_GPU_DMA_WRITE);
    }
}
