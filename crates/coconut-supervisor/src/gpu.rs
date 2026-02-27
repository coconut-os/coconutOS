//! GPU HAL shard — device selection, BAR mapping, GPU partitioning.
//!
//! Partitions the GPU's VRAM and virtual CUs for multi-shard isolation.
//! Creates a ring-3 HAL shard per partition with its own VRAM slice.
//! Dispatches compute commands through a VRAM-based command ring. Uses QEMU's
//! standard VGA (1234:1111) with CPU-simulated compute as the dispatch backend.

use crate::frame;
use crate::pci;
use crate::shard::{self, Priority};
use crate::vmm::{
    self, PTE_CACHE_DISABLE, PTE_NO_EXECUTE, PTE_USER, PTE_WRITABLE, PTE_WRITE_THROUGH,
};

/// User virtual address where the MMIO register BAR is mapped.
const GPU_MMIO_VADDR: u64 = 0x1000_0000;

/// User virtual address where the VRAM/framebuffer BAR is mapped.
const GPU_VRAM_VADDR: u64 = 0x2000_0000;

/// Maximum VRAM bytes to map (limits page table frame consumption).
const MAX_VRAM_MAP: u64 = 32 * 1024 * 1024;

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
// Embedded GPU HAL shard binary
// ---------------------------------------------------------------------------

// Ring-3 shard with a VRAM bump allocator. Validates device access, initializes
// an allocator header at VRAM base, dynamically allocates a command ring and
// three matrix buffers with typed entries, dispatches a 4×4 matmul, verifies
// the result, benchmarks 1024 matmul iterations via rdtsc, frees all
// allocations with zero-on-free, and reports via SYS_SERIAL_WRITE.
//
// Allocator header (16 bytes at VRAM + 0x00):
//   magic (4B), alloc_count (4B), next_offset (4B), total_size (4B)
// Allocation table (at VRAM + 0x10, 16 bytes per entry):
//   alloc_type (4B), offset (4B), size (4B), reserved (4B)
// Types: 1 = COMMAND_RING, 2 = DATA_BUFFER
//
// Allocations (64-byte aligned, bump):
//   0x1000  Command ring (4096 bytes)
//   0x2000  Matrix A (64 bytes)
//   0x2040  Matrix B (64 bytes)
//   0x2080  Matrix C (64 bytes)
core::arch::global_asm!(
    ".section .rodata",
    ".balign 16",
    ".global _gpu_hal_start",
    ".global _gpu_hal_end",
    "_gpu_hal_start:",

    "mov rsp, 0x800000",

    // --- Validate GPU partition config page at 0x4000 ---
    "mov r15, 0x4000",
    "cmp DWORD PTR [r15], 0x47504346",         // magic "GPCF"
    "jne 2f",

    // Load device BAR virtual bases into callee-saved registers
    "mov r13, 0x20000000",              // GPU_VRAM_VADDR
    "mov r14, 0x10000000",              // GPU_MMIO_VADDR

    // --- VRAM write/readback test (validates device access before allocator init) ---
    "mov eax, 0xDEADBEEF",
    "mov DWORD PTR [r13], eax",
    "mov ebx, DWORD PTR [r13]",
    "cmp eax, ebx",
    "jne 2f",

    // --- Read Bochs VBE ID register at MMIO BAR + 0x500 ---
    "movzx eax, WORD PTR [r14 + 0x500]",
    "cmp ax, 0xFFFF",
    "je 2f",
    "test ax, ax",
    "jz 2f",

    // --- Initialize allocator header at VRAM base ---
    "mov DWORD PTR [r13], 0x56414C4C",         // magic = "VALL"
    "mov DWORD PTR [r13 + 4], 0",              // alloc_count = 0
    "mov DWORD PTR [r13 + 8], 0x1000",         // next_offset (skip allocator page)
    // Read vram_size from config page instead of hardcoding 16 MiB
    "mov eax, DWORD PTR [r15 + 8]",
    "mov DWORD PTR [r13 + 12], eax",           // total_size = partition's VRAM size

    // --- Allocate command ring (type=1, 4096 bytes) ---
    "mov edi, 1",
    "mov esi, 4096",
    "call 40f",
    "lea r12, [r13 + rax]",            // r12 = ring VA

    // --- Allocate matrix buffers (type=2, 64 bytes each) ---
    "mov edi, 2",
    "mov esi, 64",
    "call 40f",
    "push rax",                         // save A offset

    "mov edi, 2",
    "mov esi, 64",
    "call 40f",
    "push rax",                         // save B offset

    "mov edi, 2",
    "mov esi, 64",
    "call 40f",
    "push rax",                         // save C offset

    // Stack: [rsp]=C_off, [rsp+8]=B_off, [rsp+16]=A_off

    // --- Verify allocator state ---
    "cmp DWORD PTR [r13 + 4], 4",
    "jne 2f",

    // --- Initialize command ring header at allocated location ---
    // Ring header: { magic='RING', write_ptr=0, read_ptr=0, ring_size=4096 }
    "mov DWORD PTR [r12], 0x474E4952",
    "mov DWORD PTR [r12 + 4], 0",
    "mov DWORD PTR [r12 + 8], 0",
    "mov DWORD PTR [r12 + 12], 0x1000",

    // Verify ring header readback from VRAM
    "cmp DWORD PTR [r12], 0x474E4952",
    "jne 2f",

    // --- Write Matrix A = [1, 2, ..., 16] ---
    "mov eax, DWORD PTR [rsp + 16]",
    "lea rdi, [r13 + rax]",
    "mov ecx, 1",
    "5:",
    "mov DWORD PTR [rdi], ecx",
    "add rdi, 4",
    "inc ecx",
    "cmp ecx, 17",
    "jb 5b",

    // --- Write Matrix B = 2×I₄ (identity scaled by 2) ---
    "mov eax, DWORD PTR [rsp + 8]",
    "lea rdi, [r13 + rax]",
    "xor eax, eax",
    "xor ecx, ecx",
    "6:",
    "mov DWORD PTR [rdi + rcx*4], eax",
    "inc ecx",
    "cmp ecx, 16",
    "jb 6b",
    // Diagonal entries: B[0][0], B[1][1], B[2][2], B[3][3] = 2
    "mov DWORD PTR [rdi], 2",
    "mov DWORD PTR [rdi + 20], 2",      // (1*4+1)*4 = 20
    "mov DWORD PTR [rdi + 40], 2",      // (2*4+2)*4 = 40
    "mov DWORD PTR [rdi + 60], 2",      // (3*4+3)*4 = 60

    // --- Submit DISPATCH_COMPUTE command at ring data area (header + 0x10) ---
    "lea r15, [r12 + 0x10]",
    "mov DWORD PTR [r15], 1",           // cmd_type = DISPATCH_COMPUTE
    "mov DWORD PTR [r15 + 4], 0",       // status = PENDING
    "mov eax, DWORD PTR [rsp + 16]",
    "mov DWORD PTR [r15 + 8], eax",     // src_a_offset (from allocator)
    "mov eax, DWORD PTR [rsp + 8]",
    "mov DWORD PTR [r15 + 12], eax",    // src_b_offset
    "mov eax, DWORD PTR [rsp]",
    "mov DWORD PTR [r15 + 16], eax",    // dst_c_offset
    "mov DWORD PTR [r15 + 20], 4",      // dim = 4
    "mov DWORD PTR [r15 + 24], 1",      // fence_value = 1
    "mov DWORD PTR [r15 + 28], 0",      // reserved
    // Advance write_ptr past one 32-byte entry
    "mov DWORD PTR [r12 + 4], 32",

    // --- Process command: read offsets from ring entry, compute C = A × B ---
    "mov eax, DWORD PTR [r15 + 8]",     // src_a_offset
    "lea rbx, [r13 + rax]",             // A base = VRAM + offset
    "mov eax, DWORD PTR [r15 + 12]",    // src_b_offset
    "lea r8, [r13 + rax]",              // B base
    "mov eax, DWORD PTR [r15 + 16]",    // dst_c_offset
    "lea r9, [r13 + rax]",              // C base

    // 4×4 integer matmul: C = A × B (subroutine at label 44)
    "call 44f",

    // Mark command complete and advance read_ptr
    "mov DWORD PTR [r15 + 4], 1",       // status = COMPLETE
    "mov DWORD PTR [r12 + 8], 32",      // read_ptr past one entry

    // --- Verify: C[i] == 2 * A[i] for all 16 elements ---
    "mov eax, DWORD PTR [rsp + 16]",    // A offset
    "lea rbx, [r13 + rax]",
    "xor ecx, ecx",
    "5:",
    "mov eax, DWORD PTR [rbx + rcx*4]",
    "shl eax, 1",                        // 2 * A[i]
    "cmp eax, DWORD PTR [r9 + rcx*4]",
    "jne 2f",
    "inc ecx",
    "cmp ecx, 16",
    "jb 5b",

    // --- Benchmark: 1024 matmul iterations with TSC timing ---
    // rbx = A base, r8 = B base, r9 = C base (preserved from verify)
    "rdtsc",
    "shl rdx, 32",
    "or rax, rdx",
    "mov r15, rax",                     // r15 = start TSC
    "mov ebp, 1024",                    // iteration count
    "50:",
    "call 44f",
    "dec ebp",
    "jnz 50b",
    "rdtsc",
    "shl rdx, 32",
    "or rax, rdx",
    "sub rax, r15",                     // rax = elapsed cycles

    // Print prefix: "GPU perf: 1024 iters, "
    "mov r15, rax",                     // save elapsed
    "lea rdi, [rip + 5f]",
    "mov rsi, 22",
    "mov rax, 1",                       // SYS_SERIAL_WRITE
    "syscall",

    // Convert elapsed cycle count (r15) to decimal ASCII on stack
    "mov rax, r15",
    "sub rsp, 24",                      // 24-byte buffer (enough for u64)
    "lea rdi, [rsp + 24]",             // write pointer starts at end
    "mov rbx, 10",
    "51:",
    "xor edx, edx",
    "div rbx",                          // rax = quotient, rdx = remainder
    "add dl, 0x30",                     // remainder → ASCII digit
    "dec rdi",
    "mov BYTE PTR [rdi], dl",
    "test rax, rax",
    "jnz 51b",
    // rdi = start of digits, length = (rsp + 24) - rdi
    "lea rsi, [rsp + 24]",
    "sub rsi, rdi",
    "mov rax, 1",                       // SYS_SERIAL_WRITE
    "syscall",
    "add rsp, 24",

    // Print suffix: " cycles\n"
    "lea rdi, [rip + 6f]",
    "mov rsi, 8",
    "mov rax, 1",                       // SYS_SERIAL_WRITE
    "syscall",

    // --- Free all VRAM allocations (reverse order, zero on free) ---
    "mov edi, DWORD PTR [rsp]",         // C offset
    "call 41f",
    "mov edi, DWORD PTR [rsp + 8]",     // B offset
    "call 41f",
    "mov edi, DWORD PTR [rsp + 16]",    // A offset
    "call 41f",
    "mov rax, r12",
    "sub rax, r13",
    "mov edi, eax",                      // ring offset = ring VA - VRAM base
    "call 41f",

    // --- Verify zeroing: first dword of each freed region must be 0 ---
    "mov eax, DWORD PTR [rsp]",
    "cmp DWORD PTR [r13 + rax], 0",
    "jne 2f",
    "mov eax, DWORD PTR [rsp + 8]",
    "cmp DWORD PTR [r13 + rax], 0",
    "jne 2f",
    "mov eax, DWORD PTR [rsp + 16]",
    "cmp DWORD PTR [r13 + rax], 0",
    "jne 2f",
    "cmp DWORD PTR [r12], 0",
    "jne 2f",

    // --- Zero allocator page (header + table, first 4 KiB of VRAM) ---
    "mov rdi, r13",
    "mov ecx, 512",                     // 4096 / 8 = 512 qwords
    "xor eax, eax",
    "rep stosq",

    // Clean up saved offsets from stack
    "add rsp, 24",

    // --- Success: SYS_SERIAL_WRITE ---
    "lea rdi, [rip + 3f]",
    "mov rsi, 34",                      // len("GPU mem: freed+zeroed, compute ok\n")
    "mov rax, 1",                       // SYS_SERIAL_WRITE
    "syscall",

    // SYS_EXIT(0)
    "xor edi, edi",
    "mov rax, 0",
    "syscall",
    "1: hlt",
    "jmp 1b",

    // --- Failure path ---
    "2:",
    "lea rdi, [rip + 4f]",
    "mov rsi, 18",                      // len("GPU compute: FAIL\n")
    "mov rax, 1",                       // SYS_SERIAL_WRITE
    "syscall",
    "mov edi, 1",
    "mov rax, 0",                       // SYS_EXIT(1)
    "syscall",
    "jmp 1b",

    // --- vram_alloc subroutine ---
    // Bump allocator: allocates from VRAM with 64-byte alignment.
    // Input:  edi = allocation type (1=COMMAND_RING, 2=DATA_BUFFER)
    //         esi = size in bytes
    // Output: eax = VRAM-relative offset of allocation
    // Uses:   r13 = VRAM base VA (preserved)
    // Clobbers: eax, ecx, edx
    "40:",
    "mov eax, DWORD PTR [r13 + 8]",     // next_offset
    "add eax, 63",
    "and eax, -64",                      // align up to 64 bytes (GPU cache line)
    "mov ecx, eax",
    "add ecx, esi",                      // new end = aligned + size
    "cmp ecx, DWORD PTR [r13 + 12]",    // check against total_size
    "ja 2b",                             // overflow → fail
    // Write allocation table entry at r13 + 0x10 + alloc_count * 16
    "mov edx, DWORD PTR [r13 + 4]",     // alloc_count
    "shl edx, 4",                        // * 16
    "mov DWORD PTR [r13 + rdx + 0x10], edi",  // alloc_type
    "mov DWORD PTR [r13 + rdx + 0x14], eax",  // offset
    "mov DWORD PTR [r13 + rdx + 0x18], esi",  // size
    "mov DWORD PTR [r13 + rdx + 0x1C], 0",    // reserved
    // Update header
    "mov edx, DWORD PTR [r13 + 4]",
    "inc edx",
    "mov DWORD PTR [r13 + 4], edx",     // alloc_count++
    "mov DWORD PTR [r13 + 8], ecx",     // next_offset = new end
    "ret",

    // --- vram_free subroutine ---
    // Frees a VRAM allocation: zeros the region and marks the table entry free.
    // Input:  edi = VRAM-relative offset of allocation to free
    // Output: none (jumps to failure path on error)
    // Uses:   r13 = VRAM base VA (preserved)
    // Clobbers: eax, ecx, edx, esi, rdi
    "41:",
    "xor ecx, ecx",                     // search index = 0
    "42:",
    "cmp ecx, DWORD PTR [r13 + 4]",     // index < alloc_count?
    "jae 2b",                            // not found → fail
    "mov edx, ecx",
    "shl edx, 4",                        // table offset = index * 16
    "cmp DWORD PTR [r13 + rdx + 0x14], edi",  // entry.offset == target?
    "je 43f",
    "inc ecx",
    "jmp 42b",
    "43:",
    // Found: read size, mark freed, zero region
    "mov esi, DWORD PTR [r13 + rdx + 0x18]",  // entry.size
    "mov DWORD PTR [r13 + rdx + 0x10], 0",    // alloc_type = 0 (freed)
    "mov ecx, esi",
    "shr ecx, 3",                        // qword count = size / 8
    "lea rdi, [r13 + rdi]",             // dest = VRAM_base + offset
    "xor eax, eax",
    "rep stosq",                          // zero the region
    "ret",

    // --- matmul subroutine ---
    // 4×4 integer matmul: C[i][j] = Σ_k A[i][k] * B[k][j]
    // Input:  rbx = A base VA, r8 = B base VA, r9 = C base VA
    // Clobbers: eax, ecx, edx, edi, esi, r10d, r11d
    "44:",
    "xor ecx, ecx",                     // i = 0
    "45:",
    "xor r10d, r10d",                   // j = 0
    "46:",
    "xor edi, edi",                     // accumulator = 0
    "xor r11d, r11d",                   // k = 0
    "47:",
    "mov eax, ecx",
    "shl eax, 4",                        // i * 16
    "mov edx, r11d",
    "shl edx, 2",                        // k * 4
    "add eax, edx",
    "mov esi, DWORD PTR [rbx + rax]",   // A[i][k]
    "mov eax, r11d",
    "shl eax, 4",                        // k * 16
    "mov edx, r10d",
    "shl edx, 2",                        // j * 4
    "add eax, edx",
    "imul esi, DWORD PTR [r8 + rax]",   // A[i][k] * B[k][j]
    "add edi, esi",
    "inc r11d",
    "cmp r11d, 4",
    "jb 47b",
    "mov eax, ecx",
    "shl eax, 4",
    "mov edx, r10d",
    "shl edx, 2",
    "add eax, edx",
    "mov DWORD PTR [r9 + rax], edi",    // C[i][j] = accumulator
    "inc r10d",
    "cmp r10d, 4",
    "jb 46b",
    "inc ecx",
    "cmp ecx, 4",
    "jb 45b",
    "ret",

    // String data (in code page, accessible via RIP-relative addressing)
    "3: .ascii \"GPU mem: freed+zeroed, compute ok\\n\"",
    "4: .ascii \"GPU compute: FAIL\\n\"",
    "5: .ascii \"GPU perf: 1024 iters, \"",
    "6: .ascii \" cycles\\n\"",

    "_gpu_hal_end:",
);

extern "C" {
    static _gpu_hal_start: u8;
    static _gpu_hal_end: u8;
}

fn hal_binary() -> (*const u8, *const u8) {
    (
        (&raw const _gpu_hal_start) as *const u8,
        (&raw const _gpu_hal_end) as *const u8,
    )
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
fn create_config_page(shard_id: usize, pml4_phys: u64, partition: &GpuPartition) {
    let config_phys = frame::alloc_frame_zeroed().expect("GPU config frame");

    // Write partition parameters via HHDM
    let base = vmm::phys_to_virt(config_phys);
    unsafe {
        core::ptr::write_volatile(base as *mut u32, GPU_CONFIG_MAGIC);
        core::ptr::write_volatile(base.add(4) as *mut u32, partition.id);
        core::ptr::write_volatile(base.add(8) as *mut u64, partition.vram_size);
        core::ptr::write_volatile(base.add(0x10) as *mut u32, partition.cu_count);
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
/// The shard sees VRAM at GPU_VRAM_VADDR regardless of which partition it owns.
fn map_vram_partition(
    pml4_phys: u64,
    vram_bar: &pci::BarInfo,
    partition: &GpuPartition,
) {
    let phys_start = vram_bar.phys_base + partition.vram_offset;
    let map_size = partition.vram_size.min(MAX_VRAM_MAP);
    let flags = PTE_USER | PTE_WRITABLE | PTE_NO_EXECUTE | PTE_CACHE_DISABLE | PTE_WRITE_THROUGH;

    let mut offset = 0u64;
    while offset < map_size {
        vmm::map_4k(pml4_phys, GPU_VRAM_VADDR + offset, phys_start + offset, flags);
        offset += 4096;
    }

    crate::serial_println!(
        "GPU: mapped VRAM partition {} phys {:#x}+{:#x} -> shard virt {:#x} ({} pages)",
        partition.id,
        vram_bar.phys_base,
        partition.vram_offset,
        GPU_VRAM_VADDR,
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

    // Partition VRAM and virtual CUs
    create_partitions(vram.size);

    // Create one HAL shard per partition
    let (start, end) = hal_binary();
    let count = unsafe { *(&raw const PARTITION_COUNT) };

    for pi in 0..count {
        let partition = unsafe { &(*(&raw const PARTITIONS))[pi] };
        let id = shard::create(start, end, "gpu-hal", Priority::High);
        let pml4_phys = unsafe { (*(&raw const shard::SHARDS))[id].pml4_phys };

        create_config_page(id, pml4_phys, partition);
        map_bar_to_shard(pml4_phys, &mmio, GPU_MMIO_VADDR, mmio.size, "MMIO");
        map_vram_partition(pml4_phys, &vram, partition);

        unsafe {
            (*(&raw mut PARTITIONS))[pi].owner_shard = id;
        }
        crate::serial_println!("GPU: HAL shard {} created (partition {})", id, pi);
    }
}
