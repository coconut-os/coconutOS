//! UART 16550 serial driver for debug output.
//!
//! Drives COM1 at I/O port 0x3F8, configured for 115200 baud, 8N1.

use core::arch::asm;
use core::fmt;

const COM1: u16 = 0x3F8;

// 16550 register offsets
const THR: u16 = 0; // Transmit Holding Register (write)
const IER: u16 = 1; // Interrupt Enable Register
const FCR: u16 = 2; // FIFO Control Register (write)
const LCR: u16 = 3; // Line Control Register
const MCR: u16 = 4; // Modem Control Register
const LSR: u16 = 5; // Line Status Register
const DLL: u16 = 0; // Divisor Latch Low (when DLAB=1)
const DLH: u16 = 1; // Divisor Latch High (when DLAB=1)

/// Read a byte from an x86 I/O port.
#[inline(always)]
unsafe fn port_read(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Write a byte to an x86 I/O port.
#[inline(always)]
unsafe fn port_write(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

pub struct SerialPort {
    base: u16,
}

impl SerialPort {
    /// Create a new serial port handle for the given base I/O address.
    pub const fn new(base: u16) -> Self {
        Self { base }
    }

    /// Initialize the UART: 115200 baud, 8N1, FIFOs enabled.
    pub fn init(&self) {
        unsafe {
            // Disable interrupts
            port_write(self.base + IER, 0x00);

            // Enable DLAB to set baud rate divisor
            port_write(self.base + LCR, 0x80);

            // Set divisor to 1 (115200 baud with 1.8432 MHz clock)
            port_write(self.base + DLL, 0x01);
            port_write(self.base + DLH, 0x00);

            // 8 bits, no parity, one stop bit (8N1), disable DLAB
            port_write(self.base + LCR, 0x03);

            // Enable FIFO, clear them, 14-byte threshold
            port_write(self.base + FCR, 0xC7);

            // IRQs enabled, RTS/DSR set
            port_write(self.base + MCR, 0x0B);
        }
    }

    /// Wait until the transmit holding register is empty, then send a byte.
    pub fn write_byte(&self, byte: u8) {
        unsafe {
            // Wait for transmit buffer to be empty (bit 5 of LSR)
            while (port_read(self.base + LSR) & 0x20) == 0 {
                core::hint::spin_loop();
            }
            port_write(self.base + THR, byte);
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
        Ok(())
    }
}

/// Global serial port instance (COM1).
///
/// Safety: only accessed through the macros which synchronize via the single
/// boot CPU (no SMP in milestone 0.1).
pub static mut SERIAL: SerialPort = SerialPort::new(COM1);

/// Initialize the global serial port.
pub fn init() {
    unsafe {
        (*(&raw mut SERIAL)).init();
    }
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        unsafe { write!(*(&raw mut $crate::serial::SERIAL), $($arg)*).unwrap() };
    }};
}

#[macro_export]
macro_rules! serial_println {
    ()          => { $crate::serial_print!("\n") };
    ($($arg:tt)*) => { $crate::serial_print!("{}\n", format_args!($($arg)*)) };
}
