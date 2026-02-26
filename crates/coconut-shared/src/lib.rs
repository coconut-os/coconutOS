#![no_std]

/// Magic number to validate BootInfo integrity: "CNUT" in ASCII.
pub const BOOT_INFO_MAGIC: u64 = 0x544E5543;

/// Boot handoff structure passed from bootloader to supervisor.
///
/// Placed in memory by the bootloader, pointer passed in RDI to supervisor entry.
/// All fields are plain data — no pointers to UEFI runtime structures.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    /// Must equal BOOT_INFO_MAGIC.
    pub magic: u64,
    /// Protocol version (currently 1).
    pub version: u32,
    /// Number of entries in the memory map.
    pub memory_map_count: u32,
    /// Physical address of the MemoryRegionDescriptor array.
    pub memory_map_addr: u64,
    /// Physical base address where the supervisor ELF was loaded.
    pub supervisor_phys_base: u64,
    /// Size of the supervisor image in bytes (all loaded segments).
    pub supervisor_size: u64,
}

/// Describes a contiguous physical memory region.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemoryRegionDescriptor {
    /// Physical start address (page-aligned).
    pub phys_start: u64,
    /// Size in bytes.
    pub size: u64,
    /// Type/usage of this region.
    pub region_type: MemoryRegionType,
}

/// Classification of physical memory regions.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRegionType {
    /// Free RAM, available for allocation.
    Usable = 0,
    /// Reserved by firmware or hardware — do not touch.
    Reserved = 1,
    /// ACPI tables that can be reclaimed after parsing.
    AcpiReclaimable = 2,
    /// Contains the supervisor code/data — allocated by bootloader.
    SupervisorCode = 3,
    /// Memory used by the bootloader (BootInfo, memory map) — reclaimable.
    BootloaderReclaimable = 4,
    /// ACPI NVS — must be preserved.
    AcpiNvs = 5,
    /// Memory-mapped I/O.
    Mmio = 6,
}
