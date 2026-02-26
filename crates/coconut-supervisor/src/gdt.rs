//! Global Descriptor Table with user-mode segments and TSS.
//!
//! Layout (7 entries, where TSS spans 2 slots):
//!   0x00: Null
//!   0x08: Kernel Code 64 (DPL=0)
//!   0x10: Kernel Data (DPL=0)
//!   0x18: User Data (DPL=3)   — must be before User Code for sysret
//!   0x20: User Code 64 (DPL=3)
//!   0x28: TSS Low (16-byte system segment)
//!   0x30: TSS High

use core::arch::asm;

use crate::tss;

/// A single 8-byte GDT entry.
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GdtEntry(u64);

impl GdtEntry {
    const fn null() -> Self {
        Self(0)
    }

    /// Kernel Code segment: Present, DPL=0, Code/Data, Executable, Readable, Long mode.
    const fn kernel_code() -> Self {
        // access = Present(1) | DPL=0(00) | S=1 | Exec=1 | Readable=1 = 0x9A
        // granularity: G=1 | L=1 (long mode) | limit 0xF = 0xAF
        Self(0x00AF_9A00_0000_FFFF)
    }

    /// Kernel Data segment: Present, DPL=0, Code/Data, Writable.
    const fn kernel_data() -> Self {
        // access = Present(1) | DPL=0(00) | S=1 | Writable=1 = 0x92
        // granularity: G=1 | D=1 | limit 0xF = 0xCF
        Self(0x00CF_9200_0000_FFFF)
    }

    /// User Data segment: Present, DPL=3, Code/Data, Writable.
    const fn user_data() -> Self {
        // access = Present(1) | DPL=3(11) | S=1 | Writable=1 = 0xF2
        // granularity: G=1 | D=1 | limit 0xF = 0xCF
        Self(0x00CF_F200_0000_FFFF)
    }

    /// User Code 64 segment: Present, DPL=3, Code/Data, Executable, Readable, Long mode.
    const fn user_code() -> Self {
        // access = Present(1) | DPL=3(11) | S=1 | Exec=1 | Readable=1 = 0xFA
        // granularity: G=1 | L=1 (long mode) | limit 0xF = 0xAF
        Self(0x00AF_FA00_0000_FFFF)
    }
}

/// Build the two 8-byte halves of a 16-byte TSS descriptor.
fn tss_descriptor(base: u64, limit: u16) -> (u64, u64) {
    let base_low = base & 0xFFFF;
    let base_mid = (base >> 16) & 0xFF;
    let base_mid2 = (base >> 24) & 0xFF;
    let base_high = base >> 32;
    let limit_val = limit as u64;

    // Low 8 bytes:
    //   [15:0]  limit_low
    //   [31:16] base_low
    //   [39:32] base_mid
    //   [47:40] access: Present(1) | DPL=0 | type=0x9 (64-bit TSS Available) = 0x89
    //   [51:48] limit_high (0)
    //   [55:52] flags (0)
    //   [63:56] base_mid2
    let low = limit_val
        | (base_low << 16)
        | (base_mid << 32)
        | (0x89u64 << 40)
        | (base_mid2 << 56);

    // High 8 bytes: base[63:32] in bits [31:0], rest reserved
    let high = base_high;

    (low, high)
}

/// The GDTR pointer structure loaded by `lgdt`.
#[repr(C, packed)]
struct GdtPointer {
    limit: u16,
    base: u64,
}

/// GDT storage: 7 entries (but TSS takes 2 slots, so 5 regular + 1 TSS = 7 u64s).
static mut GDT: [u64; 7] = [0; 7];

/// Kernel code segment selector (entry 1, RPL=0).
pub const KERNEL_CS: u16 = 0x08;
/// Kernel data segment selector (entry 2, RPL=0).
pub const KERNEL_DS: u16 = 0x10;
/// User data segment selector (entry 3, RPL=3).
pub const USER_DS: u16 = 0x18 | 3;
/// User code segment selector (entry 4, RPL=3).
pub const USER_CS: u16 = 0x20 | 3;
/// TSS segment selector (entry 5).
const TSS_SEL: u16 = 0x28;

/// Load the GDT with user segments and TSS, reload segment registers, load TR.
pub fn init() {
    // Initialize TSS with kernel stack top
    extern "C" {
        static __stack_top: u8;
    }
    let stack_top = (&raw const __stack_top) as u64;
    tss::init(stack_top);

    let tss_base = tss::tss_addr();
    let tss_limit = tss::tss_size() - 1;
    let (tss_low, tss_high) = tss_descriptor(tss_base, tss_limit);

    unsafe {
        let gdt = &raw mut GDT;
        (*gdt)[0] = GdtEntry::null().0;
        (*gdt)[1] = GdtEntry::kernel_code().0;
        (*gdt)[2] = GdtEntry::kernel_data().0;
        (*gdt)[3] = GdtEntry::user_data().0;
        (*gdt)[4] = GdtEntry::user_code().0;
        (*gdt)[5] = tss_low;
        (*gdt)[6] = tss_high;

        let gdt_ptr = GdtPointer {
            limit: (core::mem::size_of_val(&*gdt) - 1) as u16,
            base: (*gdt).as_ptr() as u64,
        };

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

        // Load Task Register
        asm!("ltr {0:x}", in(reg) TSS_SEL as u64, options(nostack, preserves_flags));
    }
}
