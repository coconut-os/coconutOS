//! GPU HAL shard — device selection, BAR mapping, VRAM allocation.
//!
//! Creates a ring-3 shard with a bump allocator for typed VRAM allocations.
//! Dispatches compute commands through a VRAM-based command ring. Uses QEMU's
//! standard VGA (1234:1111) with CPU-simulated compute as the dispatch backend.

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

// ---------------------------------------------------------------------------
// Embedded GPU HAL shard binary
// ---------------------------------------------------------------------------

// Ring-3 shard with a VRAM bump allocator. Validates device access, initializes
// an allocator header at VRAM base, dynamically allocates a command ring and
// three matrix buffers with typed entries, then dispatches a 4×4 matmul,
// verifies the result, and reports via SYS_SERIAL_WRITE.
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
    "mov DWORD PTR [r13 + 12], 0x01000000",    // total_size = 16 MiB

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

    // 4×4 integer matmul: C[i][j] = Σ_k A[i][k] * B[k][j]
    // Row-major layout: element [row][col] at byte offset (row*4 + col) * 4
    "xor ecx, ecx",                     // i = 0
    "7:",
    "xor r10d, r10d",                   // j = 0
    "8:",
    "xor edi, edi",                     // accumulator = 0
    "xor r11d, r11d",                   // k = 0
    "9:",
    // Load A[i][k]: offset = i*16 + k*4
    "mov eax, ecx",
    "shl eax, 4",
    "mov edx, r11d",
    "shl edx, 2",
    "add eax, edx",
    "mov esi, DWORD PTR [rbx + rax]",
    // Multiply by B[k][j]: offset = k*16 + j*4
    "mov eax, r11d",
    "shl eax, 4",
    "mov edx, r10d",
    "shl edx, 2",
    "add eax, edx",
    "imul esi, DWORD PTR [r8 + rax]",
    "add edi, esi",
    "inc r11d",
    "cmp r11d, 4",
    "jb 9b",
    // Store C[i][j]
    "mov eax, ecx",
    "shl eax, 4",
    "mov edx, r10d",
    "shl edx, 2",
    "add eax, edx",
    "mov DWORD PTR [r9 + rax], edi",
    "inc r10d",
    "cmp r10d, 4",
    "jb 8b",
    "inc ecx",
    "cmp ecx, 4",
    "jb 7b",

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

    // Clean up saved offsets from stack
    "add rsp, 24",

    // --- Success: SYS_SERIAL_WRITE ---
    "lea rdi, [rip + 3f]",
    "mov rsi, 30",                      // len("GPU mem: 4 allocs, compute ok\n")
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

    // String data (in code page, accessible via RIP-relative addressing)
    "3: .ascii \"GPU mem: 4 allocs, compute ok\\n\"",
    "4: .ascii \"GPU compute: FAIL\\n\"",

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

    // Create the HAL shard
    let (start, end) = hal_binary();
    let id = shard::create(start, end, "gpu-hal", Priority::High);

    // Map device BARs into the shard's address space
    let pml4_phys = unsafe { (*(&raw const shard::SHARDS))[id].pml4_phys };

    map_bar_to_shard(pml4_phys, &mmio, GPU_MMIO_VADDR, mmio.size, "MMIO");
    map_bar_to_shard(pml4_phys, &vram, GPU_VRAM_VADDR, MAX_VRAM_MAP, "VRAM");

    crate::serial_println!("GPU: HAL shard {} created", id);
}
