//! Serial output via `core::fmt::Write` and print macros.

use core::fmt;

use crate::sys;

/// Adapter for `core::fmt::Write` that outputs to the serial console.
pub struct SerialWriter;

impl fmt::Write for SerialWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        sys::serial_write(s.as_bytes());
        Ok(())
    }
}

/// Print to the serial console (no newline).
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        {
            use core::fmt::Write;
            let _ = write!($crate::io::SerialWriter, $($arg)*);
        }
    };
}

/// Print to the serial console with a trailing newline.
#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => {
        {
            use core::fmt::Write;
            let _ = writeln!($crate::io::SerialWriter, $($arg)*);
        }
    };
}
