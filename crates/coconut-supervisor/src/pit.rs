//! 8254 PIT (Programmable Interval Timer) driver.
//!
//! Configures channel 0 as a periodic rate generator at ~1 kHz (1 ms interval).
//! The PIT base frequency is 1,193,182 Hz; divisor 1193 gives ~1000.15 Hz.

use core::arch::asm;

// I/O ports
const PIT_CHANNEL_0: u16 = 0x40;
const PIT_COMMAND: u16 = 0x43;

// Command byte: channel 0, lo/hi access, mode 2 (rate generator), binary
const PIT_CMD_CH0_MODE2: u8 = 0x34;

// Divisor for ~1 kHz: 1,193,182 / 1193 ≈ 1000.15 Hz
const PIT_DIVISOR: u16 = 1193;

/// Global tick counter, incremented by the timer ISR.
pub static mut TICKS: u64 = 0;

#[inline(always)]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

/// Configure PIT channel 0 for periodic ~1 ms interrupts.
pub fn init() {
    unsafe {
        // Send command: channel 0, access lo/hi, mode 2, binary
        outb(PIT_COMMAND, PIT_CMD_CH0_MODE2);

        // Send divisor (low byte first, then high byte)
        outb(PIT_CHANNEL_0, (PIT_DIVISOR & 0xFF) as u8);
        outb(PIT_CHANNEL_0, (PIT_DIVISOR >> 8) as u8);
    }
}

/// Increment the tick counter. Called by the timer ISR.
pub fn increment_ticks() {
    unsafe {
        *(&raw mut TICKS) += 1;
    }
}
