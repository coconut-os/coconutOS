# Getting Started

## Prerequisites

| Tool | Purpose | Min version |
|------|---------|-------------|
| Rust (nightly) | Compiler | Managed by `rust-toolchain.toml` |
| QEMU | x86_64 emulator | 7.0+ |
| mtools | FAT32 image creation | any |
| OVMF | UEFI firmware for QEMU | Bundled with QEMU |
| mise | Task runner (optional) | 2024.0+ |

### macOS (Homebrew)

```bash
# Rust (installs nightly automatically via rust-toolchain.toml)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# QEMU + mtools (OVMF firmware ships with QEMU)
brew install qemu mtools

# mise (optional)
brew install mise
```

### Linux (apt)

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# QEMU + mtools + OVMF
sudo apt install qemu-system-x86 mtools ovmf

# mise (optional)
curl https://mise.run | sh
```

## Clone and Build

```bash
git clone https://github.com/coconut-os/coconutOS.git
cd coconutOS
```

On first build, `rustup` will install the nightly toolchain and components (`rust-src`, `llvm-tools-preview`) specified in `rust-toolchain.toml`.

### With mise

```bash
mise trust          # trust the mise.toml config
mise run build-all  # build supervisor + bootloader
mise run run        # build and boot in QEMU
```

### Without mise

```bash
./scripts/qemu-run.sh
```

This single script builds both crates and launches QEMU.

## Expected Serial Output

On a successful boot you should see output like:

```
coconutOS supervisor v0.6.0 booting...
Higher-half: page tables built, CR3 switched
GDT: loaded (7 entries, TSS active)
IDT: loaded (256 entries, higher-half)
Syscall: configured (LSTAR, STAR, SFMASK)
PIC: remapped (IRQ 0-15 -> vectors 32-47)
PIT: configured (~1ms periodic, channel 0)
Filesystem: ext2 ramdisk, 64 KiB, 1 file

Shard 0: creating (fs-reader)...

Scheduler: starting run loop
Scheduler: switching to shard 0
FS: open "/hello.txt" -> fd 0 (22 bytes)
FS: read fd 0, 22 bytes
Hello from coconutFS!
FS: close fd 0
Shard 0: sys_exit(0)
Shard 0: destroyed (memory zeroed, frames freed)

coconutOS supervisor v0.6.0: all shards completed.
Halting.
```

The fs-reader shard opens `/hello.txt` from the ext2 ramdisk, reads its contents, prints them to serial, and exits. The system halts cleanly after all shards complete.

## Next Steps

- [Building](building.md) — workspace layout, build targets, cargo configuration
- [Debugging](debugging.md) — GDB, serial output, common faults
- [Architecture](architecture.md) — full system design document
