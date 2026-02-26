#![no_std]
#![no_main]

use core::arch::{asm, naked_asm};

mod gdt;
mod idt;
mod pmm;
mod serial;

use coconut_shared::{BootInfo, BOOT_INFO_MAGIC};

/// Assembly entry point. Saves boot_info ptr, sets up the stack, zeroes BSS,
/// then calls supervisor_main. The bootloader passes BootInfo pointer in RDI.
#[unsafe(naked)]
#[no_mangle]
#[link_section = ".text._start"]
pub unsafe extern "C" fn _start() -> ! {
    // RDI = boot_info pointer from bootloader
    naked_asm!(
        // Save boot_info pointer (RDI) in RBX (callee-saved)
        "mov rbx, rdi",
        // Set up our own stack
        "lea rsp, [rip + __stack_top]",
        // Zero BSS: rdi = start, rcx = qword count
        "lea rdi, [rip + __bss_start]",
        "lea rcx, [rip + __bss_end]",
        "sub rcx, rdi",
        "shr rcx, 3",
        "xor eax, eax",
        "rep stosq",
        // Restore boot_info pointer as first argument (System V ABI)
        "mov rdi, rbx",
        // Call supervisor_main
        "call supervisor_main",
        // Should not return, but halt if it does
        "2:",
        "hlt",
        "jmp 2b",
    );
}

#[no_mangle]
pub extern "C" fn supervisor_main(boot_info_ptr: *const BootInfo) -> ! {
    // Initialize serial first so we can print
    serial::init();

    serial_println!();
    serial_println!("coconutOS supervisor v0.1.0 booting...");

    // Validate BootInfo
    let boot_info = unsafe {
        assert!(!boot_info_ptr.is_null(), "boot_info_ptr is null");
        &*boot_info_ptr
    };
    assert!(
        boot_info.magic == BOOT_INFO_MAGIC,
        "BootInfo magic mismatch: expected {:#x}, got {:#x}",
        BOOT_INFO_MAGIC,
        boot_info.magic
    );

    // Set up GDT
    gdt::init();
    serial_println!("GDT: loaded (3 entries)");

    // Set up IDT
    idt::init();
    serial_println!("IDT: loaded (256 entries)");

    // Initialize physical memory manager
    pmm::init(boot_info);

    serial_println!();
    serial_println!("coconutOS supervisor v0.1.0 initialized successfully.");
    serial_println!("Halting.");

    halt();
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
