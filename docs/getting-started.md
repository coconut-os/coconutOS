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

On a successful boot you should see output like this (abridged):

```
coconutOS supervisor v0.4.2 booting...
Higher-half: page tables built, CR3 switched
GDT: loaded (7 entries, TSS active)
IDT: loaded (256 entries, higher-half)
Syscall: configured (LSTAR, STAR, SFMASK)
PIC: remapped (IRQ 0-15 -> vectors 32-47)
PIT: configured (~1ms periodic, channel 0)
CR4: OSFXSR + TSD set
...
IOMMU: translation enabled
GPU: 2 partitions (8 MiB VRAM each, 4 CUs each)
Shard 0: creating (gpu-hal)...
Shard 0: segment 0x1000-0x3000 (R+X)
Shard 0: segment 0x3000-0x4000 (R+NX)
Shard 0: stack at virt 0x7ff000 (R+W+NX)
...
Filesystem: ext2 ramdisk, 128 KiB, 2 files
...
Scheduler: starting run loop
GPU mem: freed+zeroed, compute ok
GPU DMA: recv ok, verified
Hello from C shard!
Hello from coconutFS!
llama-inference: loaded model (dim=32, layers=2, vocab=32)
llama-inference: token 0 -> 'i'
...
llama-inference: inference complete (16 tokens)
llama-pipeline[1]: pipeline complete (16 tokens)
llama-pipeline[0]: pipeline complete (16 tokens)
...
fuzz: campaign seed=1335621000 iterations=2000 accepted=479
...
--- Fuzz Summary ---
fuzz: pass=78 fail=0 total=78
fuzz-syscall: ALL TESTS PASSED

--- Shard Profiling Summary ---
ID  Syscalls  Cycles/Syscall  Switches  Wall (ms)  Name
 0         7           74714         2          1  gpu-hal
 ...

coconutOS supervisor v0.4.2: all shards completed.
Halting.
```

The system boots, creates two GPU HAL shards (one per partition), a filesystem reader, a C FFI demo shard, a transformer inference shard, two pipeline shards that split the model across stages via IPC, and the syscall test shard. A per-shard profiling summary is printed before halt.

QEMU keeps running after `Halting.`; press Ctrl-C to exit. Shards print as they are scheduled, so the kernel's `Scheduler: switching to shard N` messages can appear in the middle of a shard's line.

### Profiling

`scripts/coconut-prof.py` turns the profiling summary into a report. It reads a saved serial log:

```bash
./scripts/qemu-run.sh > boot.log 2>&1    # Ctrl-C once "Halting." appears
python3 scripts/coconut-prof.py boot.log
```

## Next Steps

- [Writing Shards](writing-shards.md) — address space, C and Rust shards, adding a shard
- [Syscall Reference](syscalls.md) — calling convention and every syscall
- [Testing](testing.md) — what CI checks and how to reproduce it
- [Building](building.md) — workspace layout, build targets, cargo configuration
- [Debugging](debugging.md) — GDB, serial output, common faults
- [Architecture](architecture.md) — full system design document
