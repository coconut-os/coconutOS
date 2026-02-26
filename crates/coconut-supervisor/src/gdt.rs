//! Global Descriptor Table setup for 64-bit long mode.
//!
//! Three entries: null, kernel code (ring 0, 64-bit), kernel data.

use core::arch::asm;

/// A single 8-byte GDT entry.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GdtEntry {
    limit_low: u16,
    base_low: u16,
    base_mid: u8,
    access: u8,
    granularity: u8,
    base_high: u8,
}

impl GdtEntry {
    const fn null() -> Self {
        Self {
            limit_low: 0,
            base_low: 0,
            base_mid: 0,
            access: 0,
            granularity: 0,
            base_high: 0,
        }
    }

    /// Create a 64-bit code segment descriptor (ring 0).
    const fn kernel_code() -> Self {
        Self {
            limit_low: 0xFFFF,
            base_low: 0,
            base_mid: 0,
            // Present | DPL=0 | Code/Data segment | Executable | Readable
            access: 0b1001_1010,
            // Granularity=4K | Long mode | limit bits 19:16
            granularity: 0b1010_1111,
            base_high: 0,
        }
    }

    /// Create a 64-bit data segment descriptor (ring 0).
    const fn kernel_data() -> Self {
        Self {
            limit_low: 0xFFFF,
            base_low: 0,
            base_mid: 0,
            // Present | DPL=0 | Code/Data segment | Writable
            access: 0b1001_0010,
            // Granularity=4K | 32-bit (ignored in long mode for data) | limit bits
            granularity: 0b1100_1111,
            base_high: 0,
        }
    }
}

/// The GDTR pointer structure loaded by `lgdt`.
#[repr(C, packed)]
struct GdtPointer {
    limit: u16,
    base: u64,
}

/// Our GDT: null + kernel code + kernel data.
static GDT: [GdtEntry; 3] = [
    GdtEntry::null(),
    GdtEntry::kernel_code(),
    GdtEntry::kernel_data(),
];

/// Kernel code segment selector (entry 1, RPL=0).
const KERNEL_CS: u16 = 0x08;
/// Kernel data segment selector (entry 2, RPL=0).
const KERNEL_DS: u16 = 0x10;

/// Load the GDT and reload segment registers.
pub fn init() {
    let gdt_ptr = GdtPointer {
        limit: (core::mem::size_of_val(&GDT) - 1) as u16,
        base: GDT.as_ptr() as u64,
    };

    unsafe {
        // Load the GDT
        asm!("lgdt [{}]", in(reg) &gdt_ptr, options(readonly, nostack, preserves_flags));

        // Reload CS via a far return
        asm!(
            "push {cs}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            cs = in(reg) KERNEL_CS as u64,
            tmp = lateout(reg) _,
            options(preserves_flags),
        );

        // Reload data segment registers
        asm!(
            "mov ds, {0:x}",
            "mov es, {0:x}",
            "mov fs, {0:x}",
            "mov gs, {0:x}",
            "mov ss, {0:x}",
            in(reg) KERNEL_DS as u64,
            options(nostack, preserves_flags),
        );
    }
}
