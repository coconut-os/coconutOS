<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/coconut-os/.github/main/brand/lockup-dark.svg">
    <img alt="coconutOS" src="https://raw.githubusercontent.com/coconut-os/.github/main/brand/lockup.svg" height="64">
  </picture>
</p>

<p align="center"><strong>A hard shell for AI inference.</strong><br>A Rust microkernel for GPU-isolated AI inference.</p>

<p align="center">
  <a href="https://github.com/coconut-os/coconutOS/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/coconut-os/coconutOS/actions/workflows/ci.yml/badge.svg"></a>
</p>

> **Status:** v4.2.0. GPU isolation and inference stack complete; Phase 4 (hardening) in progress. Boots in QEMU, runs a transformer forward pass end-to-end, and passes its syscall test suite and a random syscall campaign in CI on every push. The GPU is QEMU's emulated VGA device with compute simulated on the CPU: the isolation logic is real, the silicon is not yet.

coconutOS is a capability-based microkernel written in Rust, designed from the ground up for secure, isolated AI inference on GPUs. The kernel runs "shards" — isolated address spaces with their own page tables — managed through unforgeable capabilities, preemptive scheduling, and IPC channels. It boots on x86-64 (QEMU/UEFI), isolates GPU partitions via IOMMU, and runs a proof-of-concept transformer inference engine as a user-mode shard.

Like a coconut, every shard sits behind a hard shell with exactly one open eye: the capability it was granted. Why it exists, where it is going and the rules it keeps are in **[Story, Vision and Ethos](docs/vision.md)**.

## Features

**Microkernel**
- UEFI boot, higher-half kernel, physical + frame allocators
- Shard isolation — per-shard page tables, ring 3 user code, W^X enforcement
- ELF shard loading — each segment mapped with its own permissions (text R+X, rodata R, data+bss R+W+NX)
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
- Per-token benchmarking via `SYS_PERF_COUNTER` with native baseline comparison

**Testing**
- Syscall boundary test shard (`fuzz-syscall`) — adversarial inputs against every syscall handler
- Seeded random syscall campaign with typed, boundary-biased arguments (syzkaller-style); every run prints its seed for exact replay
- CI builds and boots the full system in QEMU on every push and fails on kernel panics, killed shards, or test failures
- Bootable image attached to every [release](https://github.com/coconut-os/coconutOS/releases)

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

On first build, `rustup` installs the pinned nightly toolchain and components from `rust-toolchain.toml`. QEMU keeps running after the supervisor halts; press Ctrl-C to exit.

### Boot a Release Image

Each release ships a prebuilt FAT32 boot image, so you can try coconutOS with only QEMU installed:

```bash
curl -LO https://github.com/coconut-os/coconutOS/releases/latest/download/coconut-boot.img
qemu-system-x86_64 -machine q35,kernel-irqchip=split -device intel-iommu,intremap=on -m 128M \
    -drive if=pflash,format=raw,readonly=on,file=/opt/homebrew/share/qemu/edk2-x86_64-code.fd \
    -drive format=raw,file=coconut-boot.img -serial stdio -display none -no-reboot
```

On Linux, the firmware is usually at `/usr/share/OVMF/OVMF_CODE_4M.fd` or `/usr/share/OVMF/OVMF_CODE.fd`.

### Expected Output

Abridged serial output:

```
coconutOS supervisor v4.2.0 booting...
Higher-half: page tables built, CR3 switched
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
--- Inference Benchmark ---
bench: total_tokens=16 total_cycles=... dim=32 layers=2
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
 7       254           22409         7          6  fuzz-syscall

coconutOS supervisor v4.2.0: all shards completed.
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
│   │   │   ├── elf.rs         #   ELF64 PT_LOAD parsing for embedded shard images
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
│   ├── fs-reader/             # Filesystem demo shard (assembly)
│   ├── hello-c/               # C FFI demo shard (start.S + main.c)
│   ├── llama-inference/       # Transformer inference shard (start.S + main.c)
│   ├── llama-pipeline/        # Pipeline parallelism shard (start.S + main.c)
│   └── fuzz-syscall/          # Syscall boundary test shard (start.S + main.c)
├── targets/
│   ├── x86_64-coconut-shard.json  # Custom target for Rust shards
│   └── shard.ld               # Shard linker script (ELF segments at VA 0x1000)
├── docs/                      # Architecture, syscalls, shards, testing, build, debugging
├── scripts/
│   ├── qemu-run.sh            # One-command build + QEMU launch
│   ├── coconut-prof.py        # Host-side profiling report parser
│   └── bench-native.c         # Native baseline for inference benchmarks
├── SECURITY.md                # Vulnerability reporting
└── mise.toml                  # Optional task runner config
```

## Architecture Highlights

**Three-crate workspace** — the bootloader, supervisor, and shared types are compiled for different targets (`x86_64-unknown-uefi`, `x86_64-unknown-none`, and both). The supervisor has zero external runtime dependencies.

**Memory layout:**

| Region | Virtual Address | Purpose |
|--------|----------------|---------|
| Shard image | `0x1000+` | User-mode ELF: text (R+X), rodata (R), data+bss (R+W+NX) |
| Shard data | `0x100000+` | mmap'd heap (R+W+NX) |
| Shard stack | `0x7FF000` | Single 4 KiB page (R+W+NX) |
| HHDM | `0xFFFF800000000000` | Physical-to-virtual conversion |
| Kernel | `0xFFFFFFFF80000000` | Supervisor code (`code-model=kernel`) |
| MMIO | `0xFFFFFFFFC0000000` | Device register mappings |

**Syscall table** (full reference with arguments and error rules in [docs/syscalls.md](docs/syscalls.md)):

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
| 71 | `SYS_PERF_COUNTER` | Read CPU cycle counter (TSC) |

## Documentation

- **[Story, Vision and Ethos](docs/vision.md)** — why coconutOS exists, where it is going, the rules it keeps
- **[Getting Started](docs/getting-started.md)** — prerequisites, build, run, expected output
- **[Architecture](docs/architecture.md)** — full system design: shard model, GPU HAL, security, scheduler, memory, IPC, filesystem
- **[Syscall Reference](docs/syscalls.md)** — calling convention, every syscall, buffer validation, pledge
- **[Writing Shards](docs/writing-shards.md)** — address space, C and Rust shards, wiring a new shard into the build
- **[Testing](docs/testing.md)** — what CI checks, the syscall test shard, the random campaign, replaying a seed
- **[Building](docs/building.md)** — workspace layout, build targets, cargo configuration
- **[Debugging](docs/debugging.md)** — GDB, serial output, common faults

Security issues: see [SECURITY.md](SECURITY.md).

## Roadmap

| Focus | Status |
|-------|--------|
| CPU-Only Shard Model (0.1-0.6) | Complete |
| GPU Bring-Up (1.1-1.6) | Complete |
| Multi-Shard GPU Isolation (2.1-2.6) | Complete |
| Inference Stack (3.1-3.6) | Complete |
| Hardening & Multi-Vendor (4.0-4.6) | In progress |

See [.claude/ROADMAP.md](.claude/ROADMAP.md) for detailed milestones.

## License

[ISC](LICENSE)
