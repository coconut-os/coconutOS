//! GPU HAL shard — device selection, BAR mapping, user-mode hardware access.
//!
//! Creates a ring-3 shard that validates GPU device access through MMIO and
//! VRAM mappings. Uses QEMU's standard VGA device (1234:1111) as the target.

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

// Ring-3 shard that validates VRAM write/readback, reads the Bochs VBE ID
// register from MMIO, writes a command ring header into VRAM, then reports
// success or failure via SYS_SERIAL_WRITE and exits.
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

    // --- VRAM write/readback test ---
    "mov eax, 0xDEADBEEF",
    "mov DWORD PTR [r13], eax",         // write to VRAM base
    "mov ebx, DWORD PTR [r13]",         // read back
    "cmp eax, ebx",
    "jne 2f",

    // --- Read Bochs VBE ID register at MMIO BAR + 0x500 ---
    "movzx eax, WORD PTR [r14 + 0x500]",
    "cmp ax, 0xFFFF",
    "je 2f",
    "test ax, ax",
    "jz 2f",

    // --- Bump-allocate VRAM region for command ring ---
    "mov r12, 0x20001000",              // skip first page (used by write test)

    // Write command ring header: { magic='RING', wptr=0, rptr=0, size=4096 }
    "mov eax, 0x474E4952",              // 'RING' in little-endian
    "mov DWORD PTR [r12], eax",
    "mov DWORD PTR [r12 + 4], 0",
    "mov DWORD PTR [r12 + 8], 0",
    "mov DWORD PTR [r12 + 12], 0x1000",

    // Verify ring header readback from VRAM
    "cmp DWORD PTR [r12], 0x474E4952",
    "jne 2f",

    // --- Success: SYS_SERIAL_WRITE ---
    "lea rdi, [rip + 3f]",
    "mov rsi, 12",                      // len("GPU HAL: ok\n")
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
    "mov rsi, 14",                      // len("GPU HAL: FAIL\n")
    "mov rax, 1",                       // SYS_SERIAL_WRITE
    "syscall",
    "mov edi, 1",
    "mov rax, 0",                       // SYS_EXIT(1)
    "syscall",
    "jmp 1b",

    // String data (in code page, accessible via RIP-relative addressing)
    "3: .ascii \"GPU HAL: ok\\n\"",
    "4: .ascii \"GPU HAL: FAIL\\n\"",

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
