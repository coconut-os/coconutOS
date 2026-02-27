# coconut-supervisor

Freestanding microkernel for coconutOS. Manages shards (isolated address spaces), scheduling, IPC, capabilities, filesystem, GPU isolation, and hardware.

## Module Map

| File | Purpose |
|------|---------|
| `src/main.rs` | Boot trampoline (`_start`, naked asm) and `supervisor_main` initialization |
| `src/shard.rs` | Shard descriptor, lifecycle (create/destroy), mmap, user page table setup |
| `src/scheduler.rs` | Round-robin scheduler, `context_switch` (naked asm), preemption, side-channel mitigations |
| `src/syscall.rs` | STAR/LSTAR/SFMASK MSR setup, `syscall_entry` stub, syscall dispatch, buffer validation |
| `src/channel.rs` | IPC channels — single-buffered per direction, blocking receive |
| `src/capability.rs` | Per-shard capability tables — grant, revoke, restrict, inspect |
| `src/ext2.rs` | Read-only ext2 parser on `&'static [u8]` ramdisk (direct + single indirect blocks) |
| `src/fs.rs` | Open file table (16 entries), per-shard fd ownership, offset tracking |
| `src/gpu.rs` | GPU subsystem — PCIe device setup, partitioning, HAL shard creation, ASLR |
| `src/iommu.rs` | Intel VT-d IOMMU driver — DMAR parsing, root/context tables, translation enable |
| `src/pci.rs` | PCI enumeration — legacy config I/O, BAR probing, display device detection |
| `src/acpi.rs` | ACPI parser — RSDP/XSDT/RSDT, table signature lookup |
| `src/gdt.rs` | Global Descriptor Table (7 entries: null, kernel CS/DS, user DS/CS, TSS) |
| `src/idt.rs` | Interrupt Descriptor Table (256 entries), fault handlers, timer ISR (FXSAVE/FXRSTOR) |
| `src/tss.rs` | Task State Segment — stores kernel RSP for ring 3 to ring 0 transitions |
| `src/vmm.rs` | Virtual memory manager — 4-level page tables, map/unmap, HHDM, MMIO mapping |
| `src/pmm.rs` | Physical memory manager — bitmap allocator, 2 MiB regions |
| `src/frame.rs` | Frame allocator — sub-region bitmap, 4 KiB granularity |
| `src/pic.rs` | 8259A PIC driver — ICW init, IRQ remapping (0-15 to vectors 32-47), EOI |
| `src/pit.rs` | 8254 PIT driver — channel 0, mode 2, divisor 1193 (~1 kHz) |
| `src/serial.rs` | UART 16550 driver — COM1 at 0x3F8, 115200 8N1, `fmt::Write` impl |
| `src/highhalf.rs` | Post-boot cleanup — removes identity mapping from PML4[0] |
| `build.rs` | Generates 128 KiB ext2 ramdisk + model.bin at compile time, embeds shard binaries |
| `linker.ld` | Split VMA/LMA linker script — `.text.boot` at physical, rest at higher-half |

## Boot Sequence

1. **`_start`** (naked, `.text.boot` section, runs at physical addresses):
   - Saves BootInfo pointer (R12), sets temp stack at 0x300000
   - Zeroes BSS, inits serial (COM1)
   - Builds 3-region page tables (identity, HHDM, kernel) via 7-page bump allocator at 0x400000
   - Enables NXE, switches CR3, jumps to `supervisor_main` at higher-half VMA

2. **`supervisor_main`** (higher-half, normal Rust):
   - Initializes PMM, frame allocator, GDT, TSS, IDT, PIC, PIT, syscall MSRs
   - Sets CR4.OSFXSR (FXSAVE/FXRSTOR support) and CR4.TSD (rdtsc restricted to ring 0)
   - Detects CPU mitigations (IBPB support)
   - Initializes ACPI, PCI enumeration, IOMMU, GPU subsystem
   - Initializes filesystem (parses embedded ext2 ramdisk)
   - Removes identity mapping
   - Creates shards (GPU HAL ×2, fs-reader, hello-c, llama-inference)
   - Enables interrupts, enters scheduler loop

## Memory Layout

### Physical

| Address | Contents |
|---------|----------|
| `0x200000` | Supervisor code (loaded by bootloader) |
| `0x300000` | Temporary boot stack |
| `0x400000` | Boot page tables (7 pages) |
| `0x800000+` | PMM-managed free frames |

### Virtual (page table indices)

| PML4 Index | Virtual Base | Purpose |
|-----------|-------------|---------|
| 0 | `0x0000000000000000` | Identity map (boot only) / shard user pages |
| 256 | `0xFFFF800000000000` | HHDM — runtime phys-to-virt conversion |
| 511 | `0xFFFFFFFF80000000` | Kernel VMA (code-model=kernel, top 2 GiB) + MMIO |

Supervisor is linked at VMA `0xFFFFFFFF80200000`, loaded at LMA `0x200000`.
MMIO devices mapped at `0xFFFFFFFFC0000000+` (PDPT_kern[511]).

## Shard Address Space

| Region | Virtual Address | Permissions |
|--------|----------------|-------------|
| Code | `0x1000+` | R+X (multi-page) |
| Data (mmap) | `0x100000+` | R+W+NX |
| GPU BARs | `0x800000+` | R+W+NX (HAL shards only, ASLR'd) |
| Stack | `0x7FF000` | R+W+NX (single 4 KiB page) |

## Shard Lifecycle

1. **Create**: Allocate page tables, code frames, stack frame. Map code at `0x1000+` (R+X, multi-page), stack at `0x7FF000` (R+W+NX). Copy user code. Prepare synthetic kernel stack frame pointing to `shard_first_entry`.
2. **Schedule**: Round-robin across 4 priority levels. `context_switch` swaps kernel RSP. Side-channel state cleared between shards (FPU/SSE, debug registers, IBPB).
3. **First run**: `context_switch` returns into `shard_first_entry` trampoline, which sets user DS and executes `sysretq` to enter ring 3 at `0x1000`.
4. **Preemption**: PIT timer (vector 32) fires ~1 kHz. User-mode ISR saves GP regs + FXSAVE (SSE state), calls `timer_preempt`, FXRSTOR + restores GP regs, iretq.
5. **Exit**: `SYS_EXIT` syscall marks shard Exited, `schedule_yield_exit` context-switches away permanently.
6. **Destroy**: Deallocate frames and page table entries, clear capability table, zero memory.

## Syscall Table

| Number | Name | Arguments | Description |
|--------|------|-----------|-------------|
| 0 | `SYS_EXIT` | `a0`: exit code | Terminate shard |
| 1 | `SYS_SERIAL_WRITE` | `a0`: buffer ptr, `a1`: length | Write to serial console |
| 11 | `SYS_CAP_GRANT` | `a0`: handle, `a1`: target shard, `a2`: new rights | Grant capability copy to another shard |
| 12 | `SYS_CAP_REVOKE` | `a0`: handle | Revoke a capability from current shard |
| 13 | `SYS_CAP_RESTRICT` | `a0`: handle, `a1`: new rights | Restrict rights on a capability (monotonic AND) |
| 14 | `SYS_CAP_INSPECT` | `a0`: handle | Inspect capability (returns packed type/resource/rights) |
| 21 | `SYS_CHANNEL_SEND` | `a0`: channel ID, `a1`: buffer ptr, `a2`: length | Send IPC message |
| 22 | `SYS_CHANNEL_RECV` | `a0`: channel ID, `a1`: buffer ptr, `a2`: max length | Receive IPC message (blocks if empty) |
| 30 | `SYS_FS_OPEN` | `a0`: path ptr, `a1`: path length | Open file by path (returns fd) |
| 31 | `SYS_FS_READ` | `a0`: fd, `a1`: buffer ptr, `a2`: max length | Read from open file (returns bytes read) |
| 32 | `SYS_FS_STAT` | `a0`: fd | Get file size (returns size) |
| 33 | `SYS_FS_CLOSE` | `a0`: fd | Close open file (returns 0) |
| 40 | `SYS_GPU_DMA` | `a0`: target partition, `a1`: src offset, `a2`: packed(dst<<32\|len) | Inter-partition VRAM copy |
| 41 | `SYS_GPU_PLEDGE` | `a0`: bitmask of allowed categories | Monotonic syscall restriction |
| 42 | `SYS_GPU_UNVEIL` | `a0`: offset, `a1`: size | Lock VRAM range for DMA (one-shot) |
| 43 | `SYS_MMAP` | `a0`: va_start (page-aligned), `a1`: num_pages | Map data pages into shard address space |
| 62 | `SYS_YIELD` | — | Cooperative yield |

Entry: `syscall` instruction → `syscall_entry` (naked stub) → dispatch by RAX.

SFMASK clears IF on entry — no timer interrupts during syscall handling.

## Capability System

Per-shard capability table (`caps: [CapEntry; 16]`), managed entirely in kernel space. User code references capabilities by handle index (0-15).

Each `CapEntry` contains: `valid`, `cap_type`, `resource_id`, `rights`.

**Capability types:** `CAP_CHANNEL` (1), `CAP_SHARD` (2), `CAP_MEMORY` (3), `CAP_GPU_DMA` (4).

**Channel rights (bitmask):** `RIGHT_CHANNEL_SEND` (1), `RIGHT_CHANNEL_RECV` (2), `RIGHT_CHANNEL_GRANT` (4).

**GPU DMA rights:** `RIGHT_GPU_DMA_WRITE` (1).

- `SYS_CAP_GRANT`: copies a capability to another shard — requires `RIGHT_CHANNEL_GRANT`
- `SYS_CAP_REVOKE`: invalidates a capability in the current shard (non-cascading)
- `SYS_CAP_RESTRICT`: monotonically reduces rights (new rights = old AND new)
- `SYS_CAP_INSPECT`: returns packed `(cap_type << 48 | resource_id << 16 | rights)`

`CAP_CHANNEL` is enforced on `SYS_CHANNEL_SEND` and `SYS_CHANNEL_RECV`. `CAP_GPU_DMA` is enforced on `SYS_GPU_DMA`. Capabilities are cleared on shard destroy.

## GPU Isolation

- **PCIe enumeration**: Scans bus 0-255 for class 0x03 (display) devices, probes BARs
- **IOMMU**: Intel VT-d via DMAR ACPI table — root/context tables, translation enabled
- **Partitioning**: 2 partitions (VRAM carved evenly, CUs split 4/4)
- **HAL shards**: One per partition, mapped with GPU VRAM + MMIO BARs at ASLR'd addresses
- **Config page**: Read-only page at VA `0x4000` with magic, partition ID, VRAM/MMIO addresses
- **pledge/unveil**: `SYS_GPU_PLEDGE` restricts allowed syscall categories (monotonic); `SYS_GPU_UNVEIL` locks VRAM range for DMA
- **ASLR**: Per-shard randomized virtual addresses for VRAM and MMIO BARs in [0x800000, 0x3F000000)
- **Side-channel mitigations**: FPU/SSE/debug register clearing, MXCSR reset, IBPB on context switch, CR4.TSD (rdtsc #GP in ring 3)

## Filesystem

A minimal read-only ext2 filesystem, backed by a 128 KiB ramdisk generated at compile time by `build.rs`.

- **ext2.rs**: parses superblock, block group descriptor, inode table, and directory entries from a `&'static [u8]` slice. Supports direct block pointers and single indirect blocks (files up to 268 KiB).
- **fs.rs**: global open file table (`MAX_OPEN_FILES = 16`), per-shard fd ownership, offset tracking for sequential reads.
- **build.rs**: generates a rev 0 ext2 image with a root directory containing `hello.txt` and `model.bin` (~87 KiB, deterministic LCG-generated transformer weights). No external tools required.

## Interrupts and Timer

- **PIC**: 8259A dual cascade, IRQ 0-15 remapped to vectors 32-47
- **PIT**: 8254 channel 0, ~1 kHz (divisor 1193), mode 2 rate generator
- **Timer ISR** (vector 32):
  - Kernel-mode: EOI + `iretq` (no preemption)
  - User-mode: save caller-saved GP regs → FXSAVE (512 bytes SSE state) → `timer_preempt` (tick++, EOI, mark Ready, yield) → FXRSTOR → restore GP regs → `iretq`
- **Interrupt gate**: clears IF automatically — no nested interrupts
- **CR4.OSFXSR**: explicitly set during init to enable FXSAVE/FXRSTOR

## Scheduler

- **4 priority levels**: Critical, High, Normal, Low
- **Round-robin** within each level, tracked by `LAST_SCHEDULED`
- **Preemptive** via PIT timer + **cooperative** via `SYS_YIELD`
- **`context_switch`** (naked asm): pushes/pops callee-saved registers (RBX, RBP, R12-R15), swaps RSP
- **MAX_SHARDS**: 8, each with a 4 KiB kernel stack
- **Side-channel clearing**: `clear_sensitive_cpu_state()` runs before every context switch — zeros XMM0-15, resets FPU, clears debug registers, issues IBPB
