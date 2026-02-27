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

    // --- SYS_GPU_PLEDGE: restrict to SERIAL + CHANNEL + GPU_DMA (bits 0|1|2 = 7) ---
    "mov rdi, 7",
    "mov rax, 41",                      // SYS_GPU_PLEDGE
    "syscall",
    "cmp rax, 0",
    "jne 2f",

    // --- SYS_GPU_UNVEIL: expose entire partition VRAM [0, vram_size) ---
    "xor edi, edi",                     // offset = 0
    "mov esi, DWORD PTR [r15 + 8]",    // size = vram_size from config page
    "mov rax, 42",                      // SYS_GPU_UNVEIL
    "syscall",
    "cmp rax, 0",
    "jne 2f",

    // Load ASLR'd BAR virtual bases from config page
    "mov r13, QWORD PTR [r15 + 0x18]", // VRAM vaddr from config page
    "mov r14, QWORD PTR [r15 + 0x20]", // MMIO vaddr from config page

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

    // --- DMA test phase: branch on partition_id from config page ---
    "mov r15, 0x4000",                  // config page
    "mov eax, DWORD PTR [r15 + 4]",    // partition_id
    "test eax, eax",
    "jnz 60f",                          // partition 1 → receiver path

    // --- Partition 0 (sender): write test pattern, DMA to partition 1 ---
    // Write [1, 2, ..., 16] (64 bytes of u32) at VRAM + 0x100000
    "lea rdi, [r13 + 0x100000]",
    "mov ecx, 1",
    "61:",
    "mov DWORD PTR [rdi], ecx",
    "add rdi, 4",
    "inc ecx",
    "cmp ecx, 17",
    "jb 61b",

    // SYS_GPU_DMA(target=1, src_offset=0x100000, packed=(0x100000<<32)|64)
    "mov rdi, 1",                       // target partition 1
    "mov rsi, 0x100000",                // src VRAM offset
    "mov rdx, 0x100000",
    "shl rdx, 32",
    "or rdx, 64",                       // packed: dst_offset << 32 | len
    "mov rax, 40",                      // SYS_GPU_DMA
    "syscall",
    "cmp rax, 0",
    "jne 2f",                           // DMA failed

    // SYS_CHANNEL_SEND(ch=0, stack_buf, 3) — signal to partition 1
    // RSP is at 0x800000 (stack top), must allocate space below it
    "sub rsp, 8",
    "mov BYTE PTR [rsp], 0x44",         // 'D'
    "mov BYTE PTR [rsp + 1], 0x4F",     // 'O'
    "mov BYTE PTR [rsp + 2], 0x4E",     // 'N'
    "mov rdi, 0",                       // channel 0
    "mov rsi, rsp",                     // buf on stack
    "mov rdx, 3",                       // len = 3
    "mov rax, 21",                      // SYS_CHANNEL_SEND
    "syscall",
    "add rsp, 8",

    // Print "GPU DMA: sent 64 bytes to partition 1\n"
    "lea rdi, [rip + 7f]",
    "mov rsi, 38",
    "mov rax, 1",                       // SYS_SERIAL_WRITE
    "syscall",

    // SYS_EXIT(0)
    "xor edi, edi",
    "mov rax, 0",
    "syscall",
    "1: hlt",
    "jmp 1b",

    // --- Partition 1 (receiver): wait for signal, verify DMA'd data ---
    "60:",
    // SYS_CHANNEL_RECV(ch=0, stack_buf, 8) — blocks until sender signals
    // RSP is at 0x800000 (stack top), must allocate space below it
    "sub rsp, 8",
    "mov rdi, 0",                       // channel 0
    "mov rsi, rsp",                     // buf on stack
    "mov rdx, 8",                       // max_len
    "mov rax, 22",                      // SYS_CHANNEL_RECV
    "syscall",
    "add rsp, 8",

    // Verify [1, 2, ..., 16] at VRAM + 0x100000
    "lea rdi, [r13 + 0x100000]",
    "mov ecx, 1",
    "62:",
    "cmp DWORD PTR [rdi], ecx",
    "jne 2f",
    "add rdi, 4",
    "inc ecx",
    "cmp ecx, 17",
    "jb 62b",

    // Print "GPU DMA: recv ok, verified\n"
    "lea rdi, [rip + 8f]",
    "mov rsi, 27",
    "mov rax, 1",                       // SYS_SERIAL_WRITE
    "syscall",

    // SYS_EXIT(0)
    "xor edi, edi",
    "mov rax, 0",
    "syscall",
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
    "7: .ascii \"GPU DMA: sent 64 bytes to partition 1\\n\"",
    "8: .ascii \"GPU DMA: recv ok, verified\\n\"",

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
