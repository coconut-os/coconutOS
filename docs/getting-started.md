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
[BOOT] coconutOS bootloader starting
[BOOT] Loading supervisor ELF...
[SUPER] coconutOS supervisor starting
[SUPER] PMM: initialized, N regions free
[SUPER] GDT loaded
[SUPER] IDT loaded
[SUPER] PIC initialized
[SUPER] PIT initialized (1 kHz)
[SUPER] Syscall MSRs configured
[SUPER] Creating shards...
```

Followed by interleaved shard output and timer ticks. The system halts cleanly after shards exit.

## Next Steps

- [Building](building.md) — workspace layout, build targets, cargo configuration
- [Debugging](debugging.md) — GDB, serial output, common faults
- [Architecture](architecture.md) — full system design document
