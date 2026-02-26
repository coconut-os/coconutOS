#![no_std]

/// Magic number to validate BootInfo integrity: "CNUT" in ASCII.
pub const BOOT_INFO_MAGIC: u64 = 0x544E5543;

// Syscall numbers
/// Terminate the current shard. arg0 = exit code.
pub const SYS_EXIT: u64 = 0;
/// Write to serial. arg0 = buffer pointer, arg1 = length.
pub const SYS_SERIAL_WRITE: u64 = 1;
/// Send on a channel. arg0 = channel_id, arg1 = buf pointer, arg2 = length.
pub const SYS_CHANNEL_SEND: u64 = 21;
/// Receive on a channel. arg0 = channel_id, arg1 = buf pointer, arg2 = max length.
pub const SYS_CHANNEL_RECV: u64 = 22;
// Filesystem syscalls
/// Open a file by path. a0=path_ptr, a1=path_len. Returns fd.
pub const SYS_FS_OPEN: u64 = 30;
/// Read from an open file. a0=fd, a1=buf_ptr, a2=max_len. Returns bytes_read.
pub const SYS_FS_READ: u64 = 31;
/// Get file size. a0=fd. Returns file_size.
pub const SYS_FS_STAT: u64 = 32;
/// Close an open file. a0=fd. Returns 0.
pub const SYS_FS_CLOSE: u64 = 33;

/// Yield the current time slice voluntarily.
pub const SYS_YIELD: u64 = 62;

// Capability syscalls
/// Grant a capability copy to another shard. a0=handle, a1=target_shard, a2=new_rights.
pub const SYS_CAP_GRANT: u64 = 11;
/// Revoke a capability from the current shard. a0=handle.
pub const SYS_CAP_REVOKE: u64 = 12;
/// Restrict rights on a capability (monotonic reduction). a0=handle, a1=new_rights.
pub const SYS_CAP_RESTRICT: u64 = 13;
/// Inspect a capability. a0=handle. Returns packed (cap_type<<48 | resource_id<<16 | rights).
pub const SYS_CAP_INSPECT: u64 = 14;

// Capability types
pub const CAP_CHANNEL: u8 = 1;
pub const CAP_SHARD: u8 = 2;
pub const CAP_MEMORY: u8 = 3;
// 4-8 reserved for GPU, VRAM, IRQ, IO, TIMER

// Channel capability rights (bitmask)
pub const RIGHT_CHANNEL_SEND: u16 = 1 << 0;
pub const RIGHT_CHANNEL_RECV: u16 = 1 << 1;
pub const RIGHT_CHANNEL_GRANT: u16 = 1 << 2;

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
