//! Interrupt Descriptor Table with basic fault handlers.
//!
//! Sets up a 256-entry IDT. Specific handlers for divide-by-zero (#0),
//! double fault (#8), GPF (#13), and page fault (#14). All others use
//! a default handler that prints the vector number and halts.

use core::arch::{asm, naked_asm};

/// A single 16-byte IDT entry (interrupt gate descriptor) for 64-bit mode.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    _reserved: u32,
}

impl IdtEntry {
    const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            _reserved: 0,
        }
    }

    /// Create an interrupt gate entry pointing to the given handler address.
    /// selector = kernel CS = 0x08, DPL = 0, present, type = interrupt gate (0xE).
    fn new(handler: usize) -> Self {
        let addr = handler as u64;
        Self {
            offset_low: addr as u16,
            selector: 0x08,
            ist: 0,
            type_attr: 0x8E, // Present | DPL=0 | Interrupt Gate
            offset_mid: (addr >> 16) as u16,
            offset_high: (addr >> 32) as u32,
            _reserved: 0,
        }
    }
}

/// The IDTR pointer structure loaded by `lidt`.
#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u64,
}

/// 256-entry IDT (4 KiB).
static mut IDT: [IdtEntry; 256] = [IdtEntry::missing(); 256];

/// Load the IDT and install fault handlers.
pub fn init() {
    unsafe {
        // Install specific fault handlers
        IDT[0] = IdtEntry::new(isr_stub_0 as *const () as usize);
        IDT[8] = IdtEntry::new(isr_stub_8 as *const () as usize);
        IDT[13] = IdtEntry::new(isr_stub_13 as *const () as usize);
        IDT[14] = IdtEntry::new(isr_stub_14 as *const () as usize);

        // Install default handler for all other vectors
        for i in 0..256 {
            if i != 0 && i != 8 && i != 13 && i != 14 {
                IDT[i] = IdtEntry::new(isr_stub_default as *const () as usize);
            }
        }

        let idt_ptr = IdtPointer {
            limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
            base: (&raw const IDT) as u64,
        };

        asm!("lidt [{}]", in(reg) &idt_ptr, options(readonly, nostack, preserves_flags));
    }
}

// ---------------------------------------------------------------------------
// ISR stubs — naked functions that save state, call Rust handler, and halt.
// These exceptions are fatal in 0.1 so we never iretq.
// ---------------------------------------------------------------------------

/// #0 Divide Error (no error code)
#[unsafe(naked)]
unsafe extern "C" fn isr_stub_0() {
    naked_asm!(
        "push 0",    // fake error code for uniform frame
        "push 0",    // vector number
        "jmp {handler}",
        handler = sym fault_common,
    );
}

/// #8 Double Fault (error code pushed by CPU)
#[unsafe(naked)]
unsafe extern "C" fn isr_stub_8() {
    naked_asm!(
        "push 8",    // vector number (error code already on stack from CPU)
        "jmp {handler}",
        handler = sym fault_common,
    );
}

/// #13 General Protection Fault (error code pushed by CPU)
#[unsafe(naked)]
unsafe extern "C" fn isr_stub_13() {
    naked_asm!(
        "push 13",
        "jmp {handler}",
        handler = sym fault_common,
    );
}

/// #14 Page Fault (error code pushed by CPU)
#[unsafe(naked)]
unsafe extern "C" fn isr_stub_14() {
    naked_asm!(
        "push 14",
        "jmp {handler}",
        handler = sym fault_common,
    );
}

/// Default handler for all other vectors (no error code)
#[unsafe(naked)]
unsafe extern "C" fn isr_stub_default() {
    naked_asm!(
        "push 0",    // fake error code
        "push 255",  // placeholder vector
        "jmp {handler}",
        handler = sym fault_common,
    );
}

/// Common fault handler. Stack layout at entry:
///   [RSP+0]  = vector number (pushed by stub)
///   [RSP+8]  = error code (pushed by CPU or stub)
///   [RSP+16] = RIP
///   [RSP+24] = CS
///   [RSP+32] = RFLAGS
///   [RSP+40] = RSP
///   [RSP+48] = SS
#[unsafe(naked)]
unsafe extern "C" fn fault_common() {
    naked_asm!(
        // Load vector and error code, pass as arguments
        "pop rdi",          // vector number
        "pop rsi",          // error code
        "mov rdx, [rsp]",   // RIP from interrupt frame
        "call {handler}",
        "2:",
        "hlt",
        "jmp 2b",
        handler = sym fault_handler_rust,
    );
}

/// Rust-level fault handler — prints details and halts.
extern "C" fn fault_handler_rust(vector: u64, error_code: u64, rip: u64) {
    let name = match vector {
        0 => "Divide Error",
        8 => "Double Fault",
        13 => "General Protection Fault",
        14 => "Page Fault",
        _ => "Unknown Interrupt",
    };

    crate::serial_println!();
    crate::serial_println!("EXCEPTION: #{} {}", vector, name);
    crate::serial_println!("  Error code: {:#x}", error_code);
    crate::serial_println!("  RIP:        {:#x}", rip);

    if vector == 14 {
        let cr2: u64;
        unsafe { asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) };
        crate::serial_println!("  CR2:        {:#x}", cr2);
    }
}
