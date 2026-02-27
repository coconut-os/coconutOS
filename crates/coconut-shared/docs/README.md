# coconut-shared

ABI-stable types shared between the bootloader and supervisor. This crate defines the boot handoff protocol, syscall number registry, and capability constants.

## Purpose

The bootloader (`coconut-boot`, UEFI target) and supervisor (`coconut-supervisor`, freestanding target) are compiled for different targets with different ABIs. This crate provides `#[repr(C)]` types that both can agree on at the binary level.

## BootInfo Struct

Passed from bootloader to supervisor in RDI at handoff:

```rust
#[repr(C)]
pub struct BootInfo {
    pub magic: u64,               // BOOT_INFO_MAGIC (0x544E5543, "CNUT")
    pub version: u32,             // Protocol version (currently 2)
    pub memory_map_count: u32,    // Number of memory region descriptors
    pub memory_map_addr: u64,     // Physical address of descriptor array
    pub supervisor_phys_base: u64, // 0x200000
    pub supervisor_size: u64,     // Size of loaded supervisor in bytes
    pub acpi_rsdp_addr: u64,     // Physical address of ACPI RSDP (0 if not found)
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
| `SYS_CAP_GRANT` | 11 | Grant capability copy to another shard |
| `SYS_CAP_REVOKE` | 12 | Revoke a capability from current shard |
| `SYS_CAP_RESTRICT` | 13 | Restrict rights on a capability (monotonic AND) |
| `SYS_CAP_INSPECT` | 14 | Inspect capability (returns packed type/resource/rights) |
| `SYS_CHANNEL_SEND` | 21 | Send IPC message |
| `SYS_CHANNEL_RECV` | 22 | Receive IPC message (blocking) |
| `SYS_FS_OPEN` | 30 | Open file by path, returns fd |
| `SYS_FS_READ` | 31 | Read from open file, returns bytes read |
| `SYS_FS_STAT` | 32 | Get file size |
| `SYS_FS_CLOSE` | 33 | Close open file |
| `SYS_GPU_DMA` | 40 | Inter-partition VRAM copy |
| `SYS_GPU_PLEDGE` | 41 | Monotonic syscall restriction |
| `SYS_GPU_UNVEIL` | 42 | Lock VRAM range for DMA (one-shot) |
| `SYS_MMAP` | 43 | Map data pages into shard address space |
| `SYS_YIELD` | 62 | Cooperative yield to scheduler |

## GPU Pledge Constants

Bitmask values for `SYS_GPU_PLEDGE`:

| Constant | Value | Description |
|----------|-------|-------------|
| `PLEDGE_SERIAL` | 1 | Permit serial write after pledge |
| `PLEDGE_CHANNEL` | 2 | Permit channel send/recv after pledge |
| `PLEDGE_GPU_DMA` | 4 | Permit GPU DMA after pledge |

## Capability Type Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `CAP_CHANNEL` | 1 | IPC channel capability |
| `CAP_SHARD` | 2 | Shard management capability |
| `CAP_MEMORY` | 3 | Memory region capability |
| `CAP_GPU_DMA` | 4 | GPU DMA capability |

## Channel Rights Constants

Bitmask values for channel capability rights:

| Constant | Value | Description |
|----------|-------|-------------|
| `RIGHT_CHANNEL_SEND` | 1 | Permission to send on a channel |
| `RIGHT_CHANNEL_RECV` | 2 | Permission to receive on a channel |
| `RIGHT_CHANNEL_GRANT` | 4 | Permission to grant the capability to another shard |

## GPU DMA Rights Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `RIGHT_GPU_DMA_WRITE` | 1 | Permission to write to target partition via DMA |

## Constraints

- **`#![no_std]`** — no standard library dependency
- **`#[repr(C)]`** — all structs use C layout for ABI stability across targets
- **No pointers** — addresses are stored as `u64` to avoid pointer-width or provenance issues across the UEFI and freestanding targets
- **No external dependencies** — plain Rust 2021 edition, zero crate dependencies
