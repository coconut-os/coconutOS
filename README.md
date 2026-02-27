# coconutOS

A Rust microkernel for GPU-isolated AI inference.

> **Status:** GPU isolation complete, inference stack in progress — runs a transformer forward pass end-to-end.

coconutOS is a capability-based microkernel written in Rust, designed from the ground up for secure, isolated AI inference on GPUs. The kernel runs "shards" — isolated address spaces with their own page tables — managed through unforgeable capabilities, preemptive scheduling, and IPC channels. It boots on x86-64 (QEMU/UEFI), isolates GPU partitions via IOMMU, and runs a proof-of-concept transformer inference engine as a user-mode shard.

## Features

**Microkernel**
- UEFI boot, higher-half kernel, physical + frame allocators
- Shard isolation — per-shard page tables, ring 3 user code, W^X enforcement
- Preemptive round-robin scheduler (4 priority levels, PIT timer at ~1 kHz)
- IPC channels (single-buffered, blocking receive)
- Capability-based access control (grant, revoke, restrict, inspect)
- Read-only ext2 filesystem (128 KiB ramdisk with indirect blocks, generated at build time)
- SYS_MMAP for shard heap allocation

**GPU Isolation**
- PCIe enumeration with BAR decoding
- Intel VT-d IOMMU (DMAR-based translation)
- GPU partitioning — VRAM carving, CU slicing, per-partition HAL shards
- Inter-shard GPU DMA with capability-gated access
- `pledge_gpu` / `unveil_vram` — monotonic syscall restriction and VRAM range locking
- Per-shard GPU ASLR (randomized VRAM/MMIO virtual addresses)
- Side-channel mitigations — FPU/SSE/debug register clearing, CR4.TSD, IBPB

**Inference Stack**
- Rust shard runtime library (`coconut-rt`) with GPU primitives
- C ABI / FFI layer (`coconut.h` — header-only syscall wrappers)
- Proof-of-concept llama2.c transformer inference shard (model loading, RMSNorm, multi-head attention with RoPE, SiLU FFN, softmax)
- FXSAVE/FXRSTOR in timer ISR — SSE state preserved across preemption

## Quick Start

### Prerequisites

| Tool | Purpose |
|------|---------|
| Rust (nightly) | Compiler — managed by `rust-toolchain.toml` |
| QEMU | x86-64 emulator (7.0+) |
| mtools | FAT32 image creation |
| clang | C shard compilation (freestanding x86-64) |

```bash
# macOS
brew install qemu mtools llvm

# Linux (apt)
sudo apt install qemu-system-x86 mtools ovmf clang lld
```

### Build and Run

```bash
git clone https://github.com/coconut-os/coconutOS.git
cd coconutOS
./scripts/qemu-run.sh
```

On first build, `rustup` installs the nightly toolchain and components from `rust-toolchain.toml`.

### Expected Output

```
coconutOS supervisor v0.3.3 booting...
Higher-half: page tables built, CR3 switched
...
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

coconutOS supervisor v0.3.3: all shards completed.
Halting.
```

## Project Structure

```
coconutOS/
├── crates/
│   ├── coconut-boot/          # UEFI bootloader (x86_64-unknown-uefi)
│   ├── coconut-supervisor/    # Microkernel (x86_64-unknown-none)
│   │   ├── src/
│   │   │   ├── main.rs        #   Boot trampoline + supervisor_main
│   │   │   ├── shard.rs       #   Shard lifecycle, mmap, user page tables
│   │   │   ├── scheduler.rs   #   Priority round-robin, context_switch, mitigations
│   │   │   ├── syscall.rs     #   MSR setup, syscall dispatch, buffer validation
│   │   │   ├── channel.rs     #   IPC channels (single-buffered, blocking)
│   │   │   ├── capability.rs  #   Capability table, grant/revoke/restrict
│   │   │   ├── ext2.rs        #   Read-only ext2 parser (direct + indirect blocks)
│   │   │   ├── fs.rs          #   Open file table, fd management
│   │   │   ├── gpu.rs         #   GPU subsystem, partitioning, HAL shard creation
│   │   │   ├── iommu.rs       #   Intel VT-d IOMMU driver
│   │   │   ├── pci.rs         #   PCI enumeration, BAR decoding
│   │   │   ├── acpi.rs        #   RSDP/XSDT parser
│   │   │   ├── vmm.rs         #   4-level page tables, HHDM, MMIO mapping
│   │   │   ├── pmm.rs         #   Bitmap physical memory allocator
│   │   │   ├── frame.rs       #   4 KiB frame allocator
│   │   │   ├── idt.rs         #   IDT, fault handlers, timer ISR (FXSAVE/FXRSTOR)
│   │   │   └── ...            #   gdt, tss, pic, pit, serial
│   │   ├── build.rs           #   Generates ext2 ramdisk + model.bin at compile time
│   │   └── linker.ld          #   Split VMA/LMA linker script
│   ├── coconut-shared/        # Boot handoff types + syscall constants (#![no_std])
│   ├── coconut-rt/            # Shard runtime library (Rust, #![no_std])
│   └── coconut-shard-gpu/     # GPU HAL shard binary (Rust, #![no_main])
├── include/
│   └── coconut.h              # Header-only C interface to coconutOS syscalls
├── shards/
│   ├── hello-c/               # C FFI demo shard (start.S + main.c)
│   └── llama-inference/       # Transformer inference shard (start.S + main.c)
├── targets/
│   ├── x86_64-coconut-shard.json  # Custom target for Rust shards
│   └── shard.ld               # Shard linker script (flat binary at VA 0x1000)
├── docs/                      # Architecture, build, debugging docs
├── scripts/
│   └── qemu-run.sh            # One-command build + QEMU launch
└── mise.toml                  # Optional task runner config
```

## Architecture Highlights

**Three-crate workspace** — the bootloader, supervisor, and shared types are compiled for different targets (`x86_64-unknown-uefi`, `x86_64-unknown-none`, and both). The supervisor has zero external runtime dependencies.

**Memory layout:**

| Region | Virtual Address | Purpose |
|--------|----------------|---------|
| Shard code | `0x1000+` | User-mode shard binary (R+X) |
| Shard data | `0x100000+` | mmap'd heap (R+W+NX) |
| Shard stack | `0x7FF000` | Single 4 KiB page (R+W+NX) |
| HHDM | `0xFFFF800000000000` | Physical-to-virtual conversion |
| Kernel | `0xFFFFFFFF80000000` | Supervisor code (`code-model=kernel`) |
| MMIO | `0xFFFFFFFFC0000000` | Device register mappings |

**Syscall table:**

| # | Name | Description |
|---|------|-------------|
| 0 | `SYS_EXIT` | Terminate shard |
| 1 | `SYS_SERIAL_WRITE` | Write to serial console |
| 11-14 | `SYS_CAP_*` | Capability grant/revoke/restrict/inspect |
| 21-22 | `SYS_CHANNEL_*` | IPC send/receive (blocking) |
| 30-33 | `SYS_FS_*` | File open/read/stat/close |
| 40 | `SYS_GPU_DMA` | Inter-partition VRAM copy |
| 41 | `SYS_GPU_PLEDGE` | Monotonic syscall restriction |
| 42 | `SYS_GPU_UNVEIL` | Lock VRAM range for DMA |
| 43 | `SYS_MMAP` | Map data pages into shard address space |
| 62 | `SYS_YIELD` | Cooperative yield |

## Documentation

- **[Architecture](docs/architecture.md)** — full system design: shard model, GPU HAL, security, scheduler, memory, IPC, filesystem
- **[Getting Started](docs/getting-started.md)** — prerequisites, build, run, expected output
- **[Building](docs/building.md)** — workspace layout, build targets, cargo configuration
- **[Debugging](docs/debugging.md)** — GDB, serial output, common faults

## Roadmap

| Focus | Status |
|-------|--------|
| CPU-Only Shard Model (0.1-0.6) | Complete |
| GPU Bring-Up (1.1-1.6) | Complete |
| Multi-Shard GPU Isolation (2.1-2.6) | Complete |
| Inference Stack (3.1-3.3) | In Progress |
| Hardening & Multi-Vendor | Planned |

See [.claude/ROADMAP.md](.claude/ROADMAP.md) for detailed milestones.

## License

[ISC](LICENSE)
