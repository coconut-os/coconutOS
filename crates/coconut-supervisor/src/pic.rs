//! 8259A PIC (Programmable Interrupt Controller) driver.
//!
//! Remaps IRQ 0-15 from vectors 8-15/70-77 (BIOS defaults) to vectors 32-47
//! to avoid collision with CPU exception vectors 0-31.

use core::arch::asm;

// I/O port addresses
const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

// ICW1 flags
const ICW1_INIT: u8 = 0x11; // Init + ICW4 needed

// ICW4 flags
const ICW4_8086: u8 = 0x01; // 8086 mode

// OCW2: End of Interrupt
const EOI: u8 = 0x20;

/// Base vector for master PIC (IRQ 0-7 → vectors 32-39).
pub const PIC1_OFFSET: u8 = 32;
/// Base vector for slave PIC (IRQ 8-15 → vectors 40-47).
pub const PIC2_OFFSET: u8 = 40;

#[inline(always)]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") val, options(nomem, nostack, preserves_flags));
    }
    val
}

/// Small I/O delay (read from unused port 0x80).
#[inline(always)]
unsafe fn io_wait() {
    unsafe {
        asm!("out 0x80, al", in("al") 0u8, options(nomem, nostack, preserves_flags));
    }
}

/// Initialize both 8259A PICs with ICW1-4 sequence, remap to vectors 32-47, mask all IRQs.
pub fn init() {
    unsafe {
        // Save existing masks
        let mask1 = inb(PIC1_DATA);
        let mask2 = inb(PIC2_DATA);
        let _ = (mask1, mask2); // discard, we'll mask everything

        // ICW1: begin init sequence (cascade mode, ICW4 needed)
        outb(PIC1_CMD, ICW1_INIT);
        io_wait();
        outb(PIC2_CMD, ICW1_INIT);
        io_wait();

        // ICW2: vector offsets
        outb(PIC1_DATA, PIC1_OFFSET); // master: IRQ 0-7 → vectors 32-39
        io_wait();
        outb(PIC2_DATA, PIC2_OFFSET); // slave: IRQ 8-15 → vectors 40-47
        io_wait();

        // ICW3: cascade wiring
        outb(PIC1_DATA, 0x04); // master: slave on IRQ 2 (bit 2)
        io_wait();
        outb(PIC2_DATA, 0x02); // slave: cascade identity = 2
        io_wait();

        // ICW4: 8086 mode
        outb(PIC1_DATA, ICW4_8086);
        io_wait();
        outb(PIC2_DATA, ICW4_8086);
        io_wait();

        // Mask all IRQs (will selectively unmask later)
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
}

/// Unmask (enable) a specific IRQ line.
pub fn unmask(irq: u8) {
    let (port, irq_bit) = if irq < 8 {
        (PIC1_DATA, irq)
    } else {
        (PIC2_DATA, irq - 8)
    };

    unsafe {
        let mask = inb(port);
        outb(port, mask & !(1 << irq_bit));
    }
}

/// Send End of Interrupt for the given IRQ.
/// If the IRQ came from the slave PIC (IRQ 8-15), EOI must be sent to both.
pub fn send_eoi(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(PIC2_CMD, EOI);
        }
        outb(PIC1_CMD, EOI);
    }
}
