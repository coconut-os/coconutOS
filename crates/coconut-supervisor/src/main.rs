#![no_std]
#![no_main]
#![allow(dead_code)]

use core::arch::{asm, naked_asm};

mod acpi;
pub mod capability;
mod channel;
mod ext2;
mod frame;
mod fs;
mod gdt;
mod gpu;
mod highhalf;
mod idt;
mod iommu;
mod pci;
pub mod pic;
pub mod pit;
mod pmm;
pub mod scheduler;
mod serial;
pub mod shard;
mod syscall;
mod tss;
mod vmm;

use coconut_shared::{BootInfo, BOOT_INFO_MAGIC};

// Linker symbols
extern "C" {
    static __bss_start: u8;
    static __bss_end: u8;
    static __stack_top: u8;
    static __kernel_phys_start: u8;
    static __kernel_phys_end: u8;
}

/// Saved BootInfo pointer (physical address, set by trampoline).
static mut BOOT_INFO_PTR: u64 = 0;

/// Boot trampoline — runs at physical address in `.text.boot`.
///
/// Builds initial page tables (identity + HHDM), enables NXE, switches CR3,
/// then jumps to `supervisor_main` at its higher-half VMA.
///
/// Page table layout built here:
///   - PML4[0] → identity map first 1 GiB (survives CR3 switch)
///   - PML4[256] → HHDM: first 1 GiB at 0xFFFF_8000_0000_0000
///   Both use 2 MiB pages via PD-level entries.
///
/// Frame bump allocator: pages allocated at 0x400000+ (4 MiB), above supervisor.
#[unsafe(naked)]
#[no_mangle]
#[link_section = ".text.boot"]
pub unsafe extern "C" fn _start() -> ! {
    naked_asm!(
        // =====================================================================
        // Save boot_info pointer (RDI) in R12 (callee-saved)
        // =====================================================================
        "mov r12, rdi",

        // =====================================================================
        // Set up temporary stack at 0x300000 (3 MiB, in supervisor's region)
        // =====================================================================
        "mov rsp, 0x300000",

        // =====================================================================
        // Zero BSS. Symbols __bss_start/__bss_end are at VMA in top 2 GiB.
        // VMA base = 0xFFFFFFFF80000000 + KERNEL_PHYS
        // Convert VMA → physical: subtract 0xFFFFFFFF80000000
        // =====================================================================
        "mov rdi, offset {bss_start}",
        "mov rcx, offset {bss_end}",
        // Convert VMA → physical: subtract kernel VMA base offset
        "mov rax, 0xFFFFFFFF80000000",
        "sub rdi, rax",
        "sub rcx, rax",
        "sub rcx, rdi",
        "shr rcx, 3",
        "test rcx, rcx",
        "jz 1f",
        "xor eax, eax",
        "rep stosq",
        "1:",

        // =====================================================================
        // Initialize serial port (I/O port operations, no memory access needed)
        // COM1 = 0x3F8: disable interrupts, set 115200 baud 8N1, enable FIFO
        // =====================================================================
        "mov dx, 0x3F9",    // IER
        "xor al, al",
        "out dx, al",       // disable interrupts
        "mov dx, 0x3FB",    // LCR
        "mov al, 0x80",
        "out dx, al",       // DLAB=1
        "mov dx, 0x3F8",    // DLL
        "mov al, 1",
        "out dx, al",       // divisor low = 1 (115200 baud)
        "mov dx, 0x3F9",    // DLH
        "xor al, al",
        "out dx, al",       // divisor high = 0
        "mov dx, 0x3FB",    // LCR
        "mov al, 0x03",
        "out dx, al",       // 8N1, DLAB=0
        "mov dx, 0x3FA",    // FCR
        "mov al, 0xC7",
        "out dx, al",       // enable FIFO
        "mov dx, 0x3FC",    // MCR
        "mov al, 0x0B",
        "out dx, al",       // IRQs enabled, RTS/DSR

        // =====================================================================
        // Build page tables using a bump allocator starting at 0x400000 (4 MiB)
        //
        // Layout:
        //   PML4        = R13 + 0x0000   (1 page)
        //   PDPT_ident  = R13 + 0x1000   — for identity map (PML4[0])
        //   PD_ident    = R13 + 0x2000   — 512 × 2M pages = 1 GiB identity
        //   PDPT_hhdm   = R13 + 0x3000   — for HHDM (PML4[256])
        //   PD_hhdm     = R13 + 0x4000   — 512 × 2M pages = 1 GiB HHDM
        //   PDPT_kern   = R13 + 0x5000   — for kernel VMA (PML4[511])
        //   PD_kern     = R13 + 0x6000   — 512 × 2M pages for kernel text/data
        //
        // Total: 7 pages
        // =====================================================================
        "mov r13, 0x400000",    // bump allocator start

        // --- Zero 7 pages (7 × 4096 = 28672 bytes) ---
        "mov rdi, r13",
        "mov rcx, 7 * 512",    // 7 pages × 512 qwords
        "xor eax, eax",
        "rep stosq",

        // --- PML4[0] = PDPT_ident | PRESENT | WRITABLE ---
        "mov rax, r13",
        "add rax, 0x1000",     // PDPT_ident
        "or  rax, 0x03",      // PRESENT | WRITABLE
        "mov [r13], rax",      // PML4[0]

        // --- PML4[256] = PDPT_hhdm | PRESENT | WRITABLE ---
        "mov rax, r13",
        "add rax, 0x3000",     // PDPT_hhdm
        "or  rax, 0x03",
        "mov [r13 + 256*8], rax",

        // --- PML4[511] = PDPT_kern | PRESENT | WRITABLE ---
        "mov rax, r13",
        "add rax, 0x5000",     // PDPT_kern
        "or  rax, 0x03",
        "mov [r13 + 511*8], rax",

        // --- PDPT_ident[0] = PD_ident | PRESENT | WRITABLE ---
        "mov rax, r13",
        "add rax, 0x2000",
        "or  rax, 0x03",
        "mov rbx, r13",
        "add rbx, 0x1000",    // PDPT_ident base
        "mov [rbx], rax",

        // --- PDPT_hhdm[0] = PD_hhdm | PRESENT | WRITABLE ---
        "mov rax, r13",
        "add rax, 0x4000",
        "or  rax, 0x03",
        "mov rbx, r13",
        "add rbx, 0x3000",    // PDPT_hhdm base
        "mov [rbx], rax",

        // --- PDPT_kern[510] = PD_kern | PRESENT | WRITABLE ---
        // Kernel VMA 0xFFFFFFFF80000000: PDPT index = (0xFFFFFFFF80000000>>30) & 0x1FF = 510
        "mov rax, r13",
        "add rax, 0x6000",
        "or  rax, 0x03",
        "mov rbx, r13",
        "add rbx, 0x5000",    // PDPT_kern base
        "mov [rbx + 510*8], rax",

        // --- Fill PD_ident: 512 entries, 2 MiB identity (phys 0..1GiB) ---
        "mov rbx, r13",
        "add rbx, 0x2000",
        "xor ecx, ecx",
        "xor edx, edx",
        "3:",
        "mov rax, rdx",
        "or  rax, 0x83",      // PRESENT | WRITABLE | PAGE_SIZE (2M)
        "mov [rbx + rcx*8], rax",
        "add rdx, 0x200000",
        "inc ecx",
        "cmp ecx, 512",
        "jb 3b",

        // --- Fill PD_hhdm: same (maps phys 0..1GiB at HHDM offset) ---
        "mov rbx, r13",
        "add rbx, 0x4000",
        "xor ecx, ecx",
        "xor edx, edx",
        "4:",
        "mov rax, rdx",
        "or  rax, 0x83",
        "mov [rbx + rcx*8], rax",
        "add rdx, 0x200000",
        "inc ecx",
        "cmp ecx, 512",
        "jb 4b",

        // --- Fill PD_kern: same (maps phys 0..1GiB at kernel VMA) ---
        "mov rbx, r13",
        "add rbx, 0x6000",
        "xor ecx, ecx",
        "xor edx, edx",
        "5:",
        "mov rax, rdx",
        "or  rax, 0x83",
        "mov [rbx + rcx*8], rax",
        "add rdx, 0x200000",
        "inc ecx",
        "cmp ecx, 512",
        "jb 5b",

        // Total bump: 7 pages
        "add r13, 0x7000",

        // =====================================================================
        // Enable NXE in IA32_EFER (MSR 0xC0000080)
        // =====================================================================
        "mov ecx, 0xC0000080",
        "rdmsr",
        "or eax, 0x800",      // Set bit 11 (NXE)
        "wrmsr",

        // =====================================================================
        // Switch CR3 to our new PML4
        // =====================================================================
        "mov rax, r13",
        "sub rax, 0x7000",    // PML4 physical address (7 pages back from bump)
        "mov cr3, rax",

        // =====================================================================
        // Save PML4 physical address for later use
        // We can now access higher-half addresses via HHDM.
        // Store into the static variable (at its VMA = higher-half address).
        // =====================================================================

        // Save boot_info physical address to BOOT_INFO_PTR
        "mov rax, offset {boot_info_ptr}",
        // boot_info_ptr VMA is in higher half; HHDM is active, so VMA is accessible!
        "mov [rax], r12",

        // Save PML4 address. PML4 phys is at (r13 - 0x7000), pass in RDI.
        "mov rdi, r13",
        "sub rdi, 0x7000",

        // =====================================================================
        // Jump to supervisor_main at its higher-half VMA
        // =====================================================================
        "mov rax, offset {supervisor_main}",
        "jmp rax",

        // Should never reach here
        "9:",
        "hlt",
        "jmp 9b",

        bss_start = sym __bss_start,
        bss_end = sym __bss_end,
        boot_info_ptr = sym BOOT_INFO_PTR,
        supervisor_main = sym supervisor_main,
    );
}

/// Main supervisor initialization — runs at higher-half virtual address.
///
/// At this point, the boot trampoline has:
///   - Set up HHDM page tables (identity + higher-half for first 1 GiB)
///   - Switched CR3
///   - Stored boot_info_ptr and PML4 address
///   - RDI = PML4 physical address
#[no_mangle]
pub extern "C" fn supervisor_main(pml4_phys: u64) -> ! {
    // Switch stack to its higher-half VMA before doing anything that
    // might remove the identity mapping.
    unsafe {
        let stack_vma = (&raw const __stack_top) as u64;
        asm!("mov rsp, {}", in(reg) stack_vma, options(nostack));
    }

    // Initialize serial Rust wrapper (port is already configured by trampoline)
    serial::init();

    serial_println!();
    serial_println!("coconutOS supervisor v2.2.0 booting...");

    // Save PML4 address and mark higher-half as active
    highhalf::set_supervisor_pml4(pml4_phys);
    vmm::set_higher_half_active();
    serial_println!("Higher-half: page tables built, CR3 switched");

    // Remove identity mapping (safe now — stack is at higher-half VMA)
    highhalf::remove_identity_mapping();

    // Access BootInfo via HHDM
    let boot_info = unsafe {
        let phys = *(&raw const BOOT_INFO_PTR);
        &*((phys + vmm::HHDM_OFFSET) as *const BootInfo)
    };
    assert!(
        boot_info.magic == BOOT_INFO_MAGIC,
        "BootInfo magic mismatch"
    );

    // Initialize PMM from boot info
    pmm::init(boot_info);
    frame::init();

    // Set up GDT with TSS
    gdt::init();
    serial_println!("GDT: loaded (7 entries, TSS active)");

    // Set up IDT with higher-half handler addresses
    idt::init();
    serial_println!("IDT: loaded (256 entries, higher-half)");

    // Set up syscall/sysret MSRs
    syscall::init();
    serial_println!("Syscall: configured (LSTAR, STAR, SFMASK)");

    // Initialize PIC (remap IRQs to vectors 32-47)
    pic::init();
    serial_println!("PIC: remapped (IRQ 0-15 -> vectors 32-47)");

    // Initialize PIT (~1ms periodic timer on channel 0)
    pit::init();
    serial_println!("PIT: configured (~1ms periodic, channel 0)");

    // Discover ACPI tables (RSDP is in low memory, accessible via HHDM)
    acpi::init(boot_info.acpi_rsdp_addr);

    // Enumerate PCI devices (uses I/O ports, always available)
    pci::init();

    // Set up IOMMU if DMAR table present (requires acpi + vmm::map_mmio)
    iommu::init();

    // Initialize GPU subsystem — creates HAL shard if display device found
    gpu::init();

    // Initialize filesystem
    fs::init();

    serial_println!();

    // Create filesystem demo shard
    let (start, end) = shard::fs_reader_binary();
    shard::create(start, end, "fs-reader", shard::Priority::Normal);

    serial_println!();

    // Unmask PIT timer IRQ and enable interrupts
    pic::unmask(0);
    unsafe { asm!("sti", options(nomem, nostack)) };

    // Enter scheduler — preemptive round-robin until all shards exit
    scheduler::run_loop();
}

pub fn halt() -> ! {
    loop {
        unsafe { asm!("hlt") };
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial_println!();
    serial_println!("!!! KERNEL PANIC !!!");
    serial_println!("{}", info);
    loop {
        unsafe { asm!("hlt") };
    }
}
