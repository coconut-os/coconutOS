//! ACPI table discovery — parses RSDP and XSDT/RSDT to locate firmware tables.
//!
//! All tables are read via HHDM (they reside in low physical memory).
//! Only stores physical addresses and lengths; table-specific parsing
//! is the caller's responsibility.

use crate::vmm;

/// Maximum number of ACPI table entries we track.
const MAX_TABLES: usize = 32;

/// Cached ACPI table location: (signature, physical address, length).
static mut TABLES: [TableEntry; MAX_TABLES] = [TableEntry::empty(); MAX_TABLES];
static mut TABLE_COUNT: usize = 0;

#[derive(Clone, Copy)]
struct TableEntry {
    signature: [u8; 4],
    phys_addr: u64,
    length: u32,
}

impl TableEntry {
    const fn empty() -> Self {
        Self {
            signature: [0; 4],
            phys_addr: 0,
            length: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// RSDP structure (ACPI 2.0+)
// ---------------------------------------------------------------------------

/// Validate an 8-byte RSDP signature at the given virtual address.
fn rsdp_signature_valid(virt: *const u8) -> bool {
    let expected = b"RSD PTR ";
    for i in 0..8 {
        // Sound: virt points into HHDM-mapped ACPI memory, valid for at least 36 bytes.
        if unsafe { core::ptr::read_volatile(virt.add(i)) } != expected[i] {
            return false;
        }
    }
    true
}

/// Checksum the first `len` bytes at `virt`. Valid if sum wraps to 0.
fn checksum_valid(virt: *const u8, len: usize) -> bool {
    let mut sum: u8 = 0;
    for i in 0..len {
        // Sound: caller guarantees `virt` is valid for `len` bytes.
        sum = sum.wrapping_add(unsafe { core::ptr::read_volatile(virt.add(i)) });
    }
    sum == 0
}

/// Read a u32 at byte offset from a virtual pointer.
fn read_u32(base: *const u8, offset: usize) -> u32 {
    // Sound: caller ensures base+offset is within mapped, aligned memory.
    unsafe { core::ptr::read_volatile(base.add(offset) as *const u32) }
}

/// Read a u64 at byte offset from a virtual pointer.
fn read_u64(base: *const u8, offset: usize) -> u64 {
    // Sound: caller ensures base+offset is within mapped, aligned memory.
    unsafe { core::ptr::read_volatile(base.add(offset) as *const u64) }
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize ACPI table cache from the RSDP physical address.
///
/// Panics if RSDP signature or checksum is invalid.
pub fn init(rsdp_phys: u64) {
    if rsdp_phys == 0 {
        crate::serial_println!("ACPI: no RSDP provided, skipping");
        return;
    }

    let rsdp = vmm::phys_to_virt(rsdp_phys);

    assert!(rsdp_signature_valid(rsdp), "ACPI: invalid RSDP signature");
    assert!(checksum_valid(rsdp, 20), "ACPI: RSDP checksum failed");

    // Byte 15: revision. 0 = ACPI 1.0 (RSDT only), >= 2 = ACPI 2.0+ (XSDT preferred).
    let revision = unsafe { core::ptr::read_volatile(rsdp.add(15)) };

    crate::serial_println!("ACPI: RSDP at {:#x} (rev {})", rsdp_phys, revision);

    if revision >= 2 {
        // ACPI 2.0+: validate extended checksum, use XSDT
        assert!(checksum_valid(rsdp, 36), "ACPI: RSDP extended checksum failed");
        let xsdt_phys = read_u64(rsdp, 24); // offset 24: XsdtAddress
        parse_xsdt(xsdt_phys);
    } else {
        // ACPI 1.0: use RSDT
        let rsdt_phys = read_u32(rsdp, 16) as u64; // offset 16: RsdtAddress
        parse_rsdt(rsdt_phys);
    }

    log_tables();
}

/// Parse XSDT (64-bit table pointers).
fn parse_xsdt(xsdt_phys: u64) {
    let xsdt = vmm::phys_to_virt(xsdt_phys);
    let length = read_u32(xsdt, 4) as usize; // SDT header offset 4 = length

    assert!(checksum_valid(xsdt, length), "ACPI: XSDT checksum failed");

    // Entry array starts at offset 36 (after standard SDT header), each entry is 8 bytes
    let entry_count = (length - 36) / 8;
    crate::serial_println!("ACPI: XSDT at {:#x} ({} entries)", xsdt_phys, entry_count);

    for i in 0..entry_count {
        let table_phys = read_u64(xsdt, 36 + i * 8);
        cache_table(table_phys);
    }
}

/// Parse RSDT (32-bit table pointers).
fn parse_rsdt(rsdt_phys: u64) {
    let rsdt = vmm::phys_to_virt(rsdt_phys);
    let length = read_u32(rsdt, 4) as usize;

    assert!(checksum_valid(rsdt, length), "ACPI: RSDT checksum failed");

    let entry_count = (length - 36) / 4;
    crate::serial_println!("ACPI: RSDT at {:#x} ({} entries)", rsdt_phys, entry_count);

    for i in 0..entry_count {
        let table_phys = read_u32(rsdt, 36 + i * 4) as u64;
        cache_table(table_phys);
    }
}

/// Read a table's SDT header and cache its signature, address, and length.
fn cache_table(phys: u64) {
    unsafe {
        let count = *(&raw const TABLE_COUNT);
        if count >= MAX_TABLES {
            return;
        }

        let header = vmm::phys_to_virt(phys);
        let mut sig = [0u8; 4];
        for i in 0..4 {
            sig[i] = core::ptr::read_volatile(header.add(i));
        }
        let length = read_u32(header, 4);

        (*(&raw mut TABLES))[count] = TableEntry {
            signature: sig,
            phys_addr: phys,
            length,
        };
        *(&raw mut TABLE_COUNT) = count + 1;
    }
}

/// Find a cached ACPI table by its 4-byte signature.
/// Returns (physical address, length) or None.
pub fn find_table(signature: &[u8; 4]) -> Option<(u64, u32)> {
    unsafe {
        let count = *(&raw const TABLE_COUNT);
        for i in 0..count {
            let entry = &(*(&raw const TABLES))[i];
            if entry.signature == *signature {
                return Some((entry.phys_addr, entry.length));
            }
        }
    }
    None
}

/// Log all discovered table signatures to serial.
fn log_tables() {
    let count = unsafe { *(&raw const TABLE_COUNT) };
    crate::serial_print!("ACPI: tables:");
    for i in 0..count {
        let sig = unsafe { &(*(&raw const TABLES))[i].signature };
        // Signatures are always printable ASCII
        crate::serial_print!(
            " {}",
            core::str::from_utf8(sig).unwrap_or("????")
        );
    }
    crate::serial_println!();
}
