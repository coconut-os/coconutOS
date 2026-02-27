# Getting Started

## Prerequisites

| Tool | Purpose | Min version |
|------|---------|-------------|
| Rust (nightly) | Compiler | Managed by `rust-toolchain.toml` |
| QEMU | x86_64 emulator | 7.0+ |
| mtools | FAT32 image creation | any |
| clang | C shard compilation (freestanding x86-64) | 14+ |
| OVMF | UEFI firmware for QEMU | Bundled with QEMU |
| mise | Task runner (optional) | 2024.0+ |

### macOS (Homebrew)

```bash
# Rust (installs nightly automatically via rust-toolchain.toml)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# QEMU + mtools + clang (OVMF firmware ships with QEMU)
brew install qemu mtools llvm

# mise (optional)
brew install mise
```

### Linux (apt)

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# QEMU + mtools + OVMF + clang
sudo apt install qemu-system-x86 mtools ovmf clang lld

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

This single script builds all Rust crates and C shards, then launches QEMU.

## Expected Serial Output

On a successful boot you should see output like:

```
coconutOS supervisor v3.3.0 booting...
Higher-half: page tables built, CR3 switched
GDT: loaded (7 entries, TSS active)
IDT: loaded (256 entries, higher-half)
Syscall: configured (LSTAR, STAR, SFMASK)
PIC: remapped (IRQ 0-15 -> vectors 32-47)
PIT: configured (~1ms periodic, channel 0)
CR4: OSFXSR + TSD set
IOMMU: translation enabled
GPU: 2 partitions (8 MiB VRAM each, 4 CUs each)
Filesystem: ext2 ramdisk, 128 KiB, 2 files

Scheduler: starting run loop
...
GPU mem: freed+zeroed, compute ok
GPU DMA: recv ok, verified
Hello from coconutFS!
Hello from C shard!
llama-inference: loaded model (dim=32, layers=2, vocab=32)
llama-inference: token 0 -> 'i'
llama-inference: token 1 -> 't'
...
llama-inference: inference complete (16 tokens)

coconutOS supervisor v3.3.0: all shards completed.
Halting.
```

The system boots, creates GPU HAL shards (2 partitions), a filesystem reader shard, a C FFI demo shard, and a transformer inference shard. The inference shard loads a model from the ext2 ramdisk, runs a 16-token forward pass, and exits. The system halts cleanly after all shards complete.

## Next Steps

- [Building](building.md) — workspace layout, build targets, cargo configuration
- [Debugging](debugging.md) — GDB, serial output, common faults
- [Architecture](architecture.md) — full system design document
