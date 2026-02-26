//! Intel VT-d IOMMU driver — DMA remapping hardware setup.
//!
//! Parses the ACPI DMAR table to locate IOMMU hardware units (DRHDs),
//! maps their registers, allocates root/context tables, and enables
//! DMA translation. All context entries start zeroed = all DMA blocked.

use crate::{acpi, frame, vmm};

// ---------------------------------------------------------------------------
// DMAR table structures (ACPI)
// ---------------------------------------------------------------------------

/// DMAR remapping structure type: DMA Remapping Hardware Unit Definition.
const DMAR_TYPE_DRHD: u16 = 0;

/// DRHD flag: this unit handles all PCI devices not claimed by other DRHDs.
const DRHD_FLAG_INCLUDE_PCI_ALL: u8 = 0x01;

// ---------------------------------------------------------------------------
// VT-d register offsets (relative to MMIO base)
// ---------------------------------------------------------------------------

const REG_VER: usize = 0x000;     // Version
const REG_CAP: usize = 0x008;     // Capability
const REG_ECAP: usize = 0x010;    // Extended capability
const REG_GCMD: usize = 0x018;    // Global command
const REG_GSTS: usize = 0x01C;    // Global status
const REG_RTADDR: usize = 0x020;  // Root table address

// GCMD bits
const GCMD_SRTP: u32 = 1 << 30;   // Set Root Table Pointer
const GCMD_TE: u32 = 1 << 31;     // Translation Enable

// GSTS bits
const GSTS_RTPS: u32 = 1 << 30;   // Root Table Pointer Status
const GSTS_TES: u32 = 1 << 31;    // Translation Enable Status

// ---------------------------------------------------------------------------
// Register access helpers
// ---------------------------------------------------------------------------

/// Volatile 32-bit read from an MMIO register.
fn read32(base: *mut u8, offset: usize) -> u32 {
    // Sound: base is a valid MMIO mapping from vmm::map_mmio, offset is within
    // the mapped VT-d register page.
    unsafe { core::ptr::read_volatile(base.add(offset) as *const u32) }
}

/// Volatile 64-bit read from an MMIO register.
fn read64(base: *mut u8, offset: usize) -> u64 {
    // Sound: same as read32; VT-d capability registers are 64-bit aligned.
    unsafe { core::ptr::read_volatile(base.add(offset) as *const u64) }
}

/// Volatile 32-bit write to an MMIO register.
fn write32(base: *mut u8, offset: usize, val: u32) {
    // Sound: base is a valid MMIO mapping, offset within VT-d register space.
    unsafe { core::ptr::write_volatile(base.add(offset) as *mut u32, val) }
}

/// Volatile 64-bit write to an MMIO register.
fn write64(base: *mut u8, offset: usize, val: u64) {
    // Sound: same as write32; RTADDR is a 64-bit register.
    unsafe { core::ptr::write_volatile(base.add(offset) as *mut u64, val) }
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize the IOMMU by parsing DMAR, mapping registers, and enabling translation.
///
/// Does nothing if DMAR table is not present (no IOMMU hardware).
pub fn init() {
    let (dmar_phys, dmar_len) = match acpi::find_table(b"DMAR") {
        Some(v) => v,
        None => {
            crate::serial_println!("IOMMU: no DMAR table found, skipping");
            return;
        }
    };

    let dmar = vmm::phys_to_virt(dmar_phys);

    // DMAR header: standard ACPI SDT header (36 bytes) + host_address_width (1 byte)
    // + flags (1 byte), then remapping structures start at offset 48.
    let total_len = dmar_len as usize;

    // Walk remapping structures
    let mut offset = 48usize;
    while offset + 4 <= total_len {
        // Sound: dmar points to HHDM-mapped ACPI memory, valid for dmar_len bytes.
        let struct_type = unsafe { core::ptr::read_volatile(dmar.add(offset) as *const u16) };
        let struct_len =
            unsafe { core::ptr::read_volatile(dmar.add(offset + 2) as *const u16) } as usize;

        if struct_len < 4 || offset + struct_len > total_len {
            break;
        }

        if struct_type == DMAR_TYPE_DRHD {
            process_drhd(dmar, offset, struct_len);
        }

        offset += struct_len;
    }
}

/// Process a single DRHD (DMA Remapping Hardware Unit Definition).
fn process_drhd(dmar: *mut u8, offset: usize, _len: usize) {
    // DRHD layout after type+length (offset+4):
    //   byte 0: flags
    //   byte 2-3: segment number (u16)
    //   byte 4-11: register base address (u64)
    let flags = unsafe { core::ptr::read_volatile(dmar.add(offset + 4)) };
    let reg_base_phys =
        unsafe { core::ptr::read_volatile(dmar.add(offset + 8) as *const u64) };

    let scope = if flags & DRHD_FLAG_INCLUDE_PCI_ALL != 0 {
        "INCLUDE_PCI_ALL"
    } else {
        "scoped"
    };
    crate::serial_println!("IOMMU: DRHD at {:#x} ({})", reg_base_phys, scope);

    // Map the VT-d register page (4 KiB is sufficient for the base registers)
    let regs = vmm::map_mmio(reg_base_phys, 4096);

    // Read version and capabilities
    let ver = read32(regs, REG_VER);
    let ver_major = (ver >> 4) & 0xF;
    let ver_minor = ver & 0xF;
    let cap = read64(regs, REG_CAP);
    let ecap = read64(regs, REG_ECAP);

    crate::serial_println!("IOMMU: VT-d version {}.{}", ver_major, ver_minor);
    crate::serial_println!("IOMMU: CAP={:#018x} ECAP={:#018x}", cap, ecap);

    // Allocate root table (4 KiB zeroed frame = 256 root entries, all zero = no context)
    let root_table_phys =
        frame::alloc_frame_zeroed().expect("IOMMU: failed to allocate root table");

    // Allocate one context table (4 KiB zeroed = all entries invalid = all DMA blocked)
    let _context_table_phys =
        frame::alloc_frame_zeroed().expect("IOMMU: failed to allocate context table");

    // Set root table address (bits 63:12 = physical address of root table, bit 11:0 = 0 for legacy mode)
    write64(regs, REG_RTADDR, root_table_phys);

    // Issue Set Root Table Pointer command
    write32(regs, REG_GCMD, GCMD_SRTP);

    // Wait for RTPS (root table pointer status) in GSTS
    let mut timeout = 100_000u32;
    while read32(regs, REG_GSTS) & GSTS_RTPS == 0 {
        timeout -= 1;
        assert!(timeout > 0, "IOMMU: SRTP command timed out");
        core::hint::spin_loop();
    }

    // Enable translation
    let gsts = read32(regs, REG_GSTS);
    write32(regs, REG_GCMD, gsts | GCMD_TE);

    // Wait for TES (translation enable status)
    timeout = 100_000;
    while read32(regs, REG_GSTS) & GSTS_TES == 0 {
        timeout -= 1;
        assert!(timeout > 0, "IOMMU: TE command timed out");
        core::hint::spin_loop();
    }

    crate::serial_println!("IOMMU: translation enabled");
}
