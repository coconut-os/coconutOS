# coconut-boot

UEFI bootloader for coconutOS. Loads the supervisor ELF from the boot filesystem and transfers control to it.

## Boot Flow

1. **UEFI entry** — `efi_main` initializes UEFI services and serial output
2. **Load supervisor** — Opens `\EFI\coconut\supervisor.elf` from the boot filesystem
3. **Parse ELF** — Extracts `PT_LOAD` segments, copies them to physical address `0x200000`
4. **Build BootInfo** — Allocates 2 pages for the `BootInfo` struct and memory map array, translates the UEFI memory map into `MemoryRegionDescriptor` entries
5. **Exit Boot Services** — No more UEFI runtime calls after this point
6. **Jump to supervisor** — Inline assembly sets RDI to the `BootInfo` pointer and jumps to the supervisor entry point

## ABI Bridge

UEFI uses the **Microsoft x64** calling convention (first argument in RCX), while the supervisor expects **System V AMD64** (first argument in RDI). The bootloader bridges this with inline assembly that places the `BootInfo` pointer in RDI before jumping.

## Module Overview

| File | Purpose |
|------|---------|
| `src/main.rs` | UEFI entry point, ELF loading, memory map construction, supervisor handoff |
| `src/elf.rs` | Minimal ELF64 parser — `Elf64Header`, `Elf64Phdr`, `PT_LOAD` segment extraction |

## Dependencies

- `uefi` v0.33 (features: `panic_handler`, `alloc`, `global_allocator`)
- `coconut-shared` (workspace — `BootInfo`, `MemoryRegionDescriptor`)

## Build Target

```
x86_64-unknown-uefi
```

Output: `target/x86_64-unknown-uefi/release/coconut-boot.efi` (PE32+ UEFI application)

The `qemu-run.sh` script places this at `\EFI\BOOT\BOOTX64.EFI` on the FAT32 disk image.
