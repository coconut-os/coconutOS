# coconut-shared

ABI-stable types shared between the bootloader and supervisor. This crate defines the boot handoff protocol and syscall number registry.

## Purpose

The bootloader (`coconut-boot`, UEFI target) and supervisor (`coconut-supervisor`, freestanding target) are compiled for different targets with different ABIs. This crate provides `#[repr(C)]` types that both can agree on at the binary level.

## BootInfo Struct

Passed from bootloader to supervisor in RDI at handoff:

```rust
#[repr(C)]
pub struct BootInfo {
    pub magic: u64,               // BOOT_INFO_MAGIC (0x544E5543, "CNUT")
    pub version: u32,             // Protocol version (currently 1)
    pub memory_map_count: u32,    // Number of memory region descriptors
    pub memory_map_addr: u64,     // Physical address of descriptor array
    pub supervisor_phys_base: u64, // 0x200000
    pub supervisor_size: u64,     // Size of loaded supervisor in bytes
}
```

## Memory Region Descriptors

```rust
#[repr(C)]
pub struct MemoryRegionDescriptor {
    pub phys_start: u64,
    pub size: u64,
    pub region_type: MemoryRegionType,
}

#[repr(u32)]
pub enum MemoryRegionType {
    Usable = 0,
    Reserved = 1,
    AcpiReclaimable = 2,
    SupervisorCode = 3,
    BootloaderReclaimable = 4,
    AcpiNvs = 5,
    Mmio = 6,
}
```

The bootloader translates the UEFI memory map into this format before exiting boot services.

## Syscall Number Registry

| Constant | Value | Description |
|----------|-------|-------------|
| `SYS_EXIT` | 0 | Terminate shard, arg0 = exit code |
| `SYS_SERIAL_WRITE` | 1 | Write to serial, arg0 = buffer ptr, arg1 = length |
| `SYS_CHANNEL_SEND` | 21 | Send IPC message |
| `SYS_CHANNEL_RECV` | 22 | Receive IPC message (blocking) |
| `SYS_YIELD` | 62 | Cooperative yield to scheduler |

## Constraints

- **`#![no_std]`** — no standard library dependency
- **`#[repr(C)]`** — all structs use C layout for ABI stability across targets
- **No pointers** — addresses are stored as `u64` to avoid pointer-width or provenance issues across the UEFI and freestanding targets
- **No external dependencies** — plain Rust 2021 edition, zero crate dependencies
