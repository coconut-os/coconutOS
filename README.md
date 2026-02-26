# coconutOS

A Rust microkernel for GPU-isolated AI inference.

> **Status:** Phase 0 (CPU-Only Shard Model) complete. Phase 1 (GPU Bring-Up) next.

coconutOS is a capability-based microkernel written in Rust, designed from the ground up for secure, isolated AI inference on GPUs. The kernel runs "shards" — isolated address spaces with their own page tables — managed through unforgeable capabilities, preemptive scheduling, and IPC channels. It currently boots on x86-64 (QEMU/UEFI), runs user-mode shards in ring 3, and includes a read-only ext2 ramdisk filesystem.

## Phase 0 Milestones

- [x] **0.1** — Boot (UEFI), serial console, physical memory manager
- [x] **0.2** — Higher-half kernel, shard creation and destruction
- [x] **0.3** — IPC channels between shards (synchronous, blocking)
- [x] **0.4** — Preemptive round-robin scheduler (PIT timer, 4 priority levels)
- [x] **0.5** — Capability-based access control (grant, revoke, restrict, inspect)
- [x] **0.6** — Minimal read-only filesystem (ext2 ramdisk, build-time generated)

## Quick Start

### Prerequisites

| Tool | Purpose |
|------|---------|
| Rust (nightly) | Compiler — managed by `rust-toolchain.toml` |
| QEMU | x86-64 emulator (7.0+) |
| mtools | FAT32 image creation |

```bash
# macOS
brew install qemu mtools

# Linux (apt)
sudo apt install qemu-system-x86 mtools ovmf
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

## Project Structure

```
coconutOS/
├── crates/
│   ├── coconut-boot/          # UEFI bootloader (x86_64-unknown-uefi)
│   │   └── src/main.rs        #   ELF loader, memory map, supervisor handoff
│   ├── coconut-supervisor/    # Microkernel (x86_64-unknown-none)
│   │   ├── src/
│   │   │   ├── main.rs        #   Boot trampoline + supervisor_main
│   │   │   ├── shard.rs       #   Shard lifecycle, user page tables
│   │   │   ├── scheduler.rs   #   Priority round-robin, context_switch
│   │   │   ├── syscall.rs     #   MSR setup, syscall dispatch
│   │   │   ├── channel.rs     #   IPC channels (single-buffered, blocking)
│   │   │   ├── capability.rs  #   Capability table, grant/revoke/restrict
│   │   │   ├── ext2.rs        #   Read-only ext2 parser
│   │   │   ├── fs.rs          #   Open file table, fd management
│   │   │   ├── vmm.rs         #   4-level page tables, HHDM
│   │   │   ├── pmm.rs         #   Bitmap physical memory allocator
│   │   │   ├── frame.rs       #   4 KiB frame allocator
│   │   │   ├── gdt.rs         #   GDT (7 entries) + TSS
│   │   │   ├── idt.rs         #   IDT (256 entries), fault/timer handlers
│   │   │   ├── pic.rs         #   8259A PIC driver
│   │   │   ├── pit.rs         #   8254 PIT driver (~1 kHz)
│   │   │   ├── serial.rs      #   UART 16550 (COM1, 115200 8N1)
│   │   │   └── ...
│   │   ├── build.rs           #   Generates ext2 ramdisk at compile time
│   │   └── linker.ld          #   Split VMA/LMA linker script
│   └── coconut-shared/        # Boot handoff types + syscall constants
│       └── src/lib.rs         #   BootInfo, MemoryRegionDescriptor, SYS_*
├── docs/
│   ├── architecture.md        # Full system design document
│   ├── getting-started.md     # Prerequisites, build, run
│   ├── building.md            # Workspace layout, cargo configuration
│   └── debugging.md           # GDB, serial output, common faults
├── scripts/
│   └── qemu-run.sh            # One-command build + QEMU launch
└── mise.toml                  # Optional task runner config
```

## Architecture Highlights

**Three-crate workspace** — the bootloader, supervisor, and shared types are compiled for different targets (`x86_64-unknown-uefi`, `x86_64-unknown-none`, and both).

**Memory layout:**

| Region | Virtual Address | Purpose |
|--------|----------------|---------|
| Identity map | `0x0` | Boot only; shard user pages at runtime |
| HHDM | `0xFFFF800000000000` | Physical-to-virtual conversion |
| Kernel | `0xFFFFFFFF80000000` | Supervisor code (`code-model=kernel`) |

**Syscall table:**

| # | Name | Description |
|---|------|-------------|
| 0 | `SYS_EXIT` | Terminate shard |
| 1 | `SYS_SERIAL_WRITE` | Write to serial console |
| 11 | `SYS_CAP_GRANT` | Grant capability to another shard |
| 12 | `SYS_CAP_REVOKE` | Revoke a capability |
| 13 | `SYS_CAP_RESTRICT` | Restrict capability rights |
| 14 | `SYS_CAP_INSPECT` | Inspect a capability |
| 21 | `SYS_CHANNEL_SEND` | Send IPC message |
| 22 | `SYS_CHANNEL_RECV` | Receive IPC message (blocking) |
| 30 | `SYS_FS_OPEN` | Open file by path |
| 31 | `SYS_FS_READ` | Read from open file |
| 32 | `SYS_FS_STAT` | Get file size |
| 33 | `SYS_FS_CLOSE` | Close file |
| 62 | `SYS_YIELD` | Cooperative yield |

## Documentation

- **[Architecture](docs/architecture.md)** — full system design: shard model, GPU HAL, security, scheduler, memory, IPC, filesystem
- **[Getting Started](docs/getting-started.md)** — prerequisites, build, run, expected output
- **[Building](docs/building.md)** — workspace layout, build targets, cargo configuration
- **[Debugging](docs/debugging.md)** — GDB, serial output, common faults
- **[coconut-boot](crates/coconut-boot/docs/README.md)** — UEFI bootloader internals
- **[coconut-supervisor](crates/coconut-supervisor/docs/README.md)** — microkernel module map, boot sequence, memory layout
- **[coconut-shared](crates/coconut-shared/docs/README.md)** — boot handoff types, syscall registry, capability constants

## Roadmap

| Phase | Focus | Status |
|-------|-------|--------|
| **0** | CPU-Only Shard Model | Complete |
| **1** | GPU Bring-Up (AMD RDNA3/CDNA3) | Next |
| **2** | Multi-Shard GPU Isolation | Planned |
| **3** | Inference Stack (LLM runtime) | Planned |
| **4** | Hardening & Multi-Vendor (NVIDIA, Apple) | Planned |

See the [Architecture Document](docs/architecture.md) for the full roadmap.

## License

TBD
