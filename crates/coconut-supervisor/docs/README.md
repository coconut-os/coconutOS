# coconut-supervisor

Freestanding microkernel for coconutOS. Manages shards (isolated address spaces), scheduling, IPC, and hardware.

## Module Map

| File | Purpose |
|------|---------|
| `src/main.rs` | Boot trampoline (`_start`, naked asm) and `supervisor_main` initialization |
| `src/shard.rs` | Shard descriptor, lifecycle (create/destroy), user page table setup |
| `src/scheduler.rs` | Round-robin scheduler, `context_switch` (naked asm), preemption support |
| `src/syscall.rs` | STAR/LSTAR/SFMASK MSR setup, `syscall_entry` stub, syscall dispatch |
| `src/channel.rs` | IPC channels — single-buffered per direction, blocking receive |
| `src/gdt.rs` | Global Descriptor Table (7 entries: null, kernel CS/DS, user DS/CS, TSS) |
| `src/idt.rs` | Interrupt Descriptor Table (256 entries), fault and timer handlers |
| `src/tss.rs` | Task State Segment — stores kernel RSP for ring 3 to ring 0 transitions |
| `src/vmm.rs` | Virtual memory manager — 4-level page tables, map/unmap, HHDM helpers |
| `src/pmm.rs` | Physical memory manager — bitmap allocator, 2 MiB regions |
| `src/frame.rs` | Frame allocator — sub-region bitmap, 4 KiB granularity |
| `src/pic.rs` | 8259A PIC driver — ICW init, IRQ remapping (0-15 to vectors 32-47), EOI |
| `src/pit.rs` | 8254 PIT driver — channel 0, mode 2, divisor 1193 (~1 kHz) |
| `src/serial.rs` | UART 16550 driver — COM1 at 0x3F8, 115200 8N1, `fmt::Write` impl |
| `src/highhalf.rs` | Post-boot cleanup — removes identity mapping from PML4[0] |
| `linker.ld` | Split VMA/LMA linker script — `.text.boot` at physical, rest at higher-half |

## Boot Sequence

1. **`_start`** (naked, `.text.boot` section, runs at physical addresses):
   - Saves BootInfo pointer (R12), sets temp stack at 0x300000
   - Zeroes BSS, inits serial (COM1)
   - Builds 3-region page tables (identity, HHDM, kernel) via 7-page bump allocator at 0x400000
   - Enables NXE, switches CR3, jumps to `supervisor_main` at higher-half VMA

2. **`supervisor_main`** (higher-half, normal Rust):
   - Initializes PMM, GDT, TSS, IDT, PIC, PIT, syscall MSRs
   - Removes identity mapping
   - Creates initial shards, enters scheduler loop

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
| 511 | `0xFFFFFFFF80000000` | Kernel VMA (code-model=kernel, top 2 GiB) |

Supervisor is linked at VMA `0xFFFFFFFF80200000`, loaded at LMA `0x200000`.

## Shard Lifecycle

1. **Create**: Allocate page table, code frame, stack frame. Map code at `0x1000` (R+X), stack at `0x7FF000` (R+W+NX). Copy user code. Prepare synthetic kernel stack frame pointing to `shard_first_entry`.
2. **Schedule**: Round-robin across 4 priority levels. `context_switch` swaps kernel RSP.
3. **First run**: `context_switch` returns into `shard_first_entry` trampoline, which sets user DS and executes `sysretq` to enter ring 3 at `0x1000`.
4. **Preemption**: PIT timer (vector 32) fires ~1 kHz. User-mode ISR saves state, calls `timer_preempt`, which yields to scheduler.
5. **Exit**: `SYS_EXIT` syscall marks shard Exited, `schedule_yield_exit` context-switches away permanently.
6. **Destroy**: Deallocate frames and page table entries.

## Syscall Table

| Number | Name | Arguments | Description |
|--------|------|-----------|-------------|
| 0 | `SYS_EXIT` | `arg0`: exit code | Terminate shard |
| 1 | `SYS_SERIAL_WRITE` | `arg0`: buffer ptr, `arg1`: length | Write to serial console |
| 21 | `SYS_CHANNEL_SEND` | `arg0`: channel ID, `arg1`: buffer ptr, `arg2`: length | Send IPC message |
| 22 | `SYS_CHANNEL_RECV` | `arg0`: channel ID, `arg1`: buffer ptr, `arg2`: max length | Receive IPC message (blocks if empty) |
| 62 | `SYS_YIELD` | — | Cooperative yield |

Entry: `syscall` instruction → `syscall_entry` (naked stub) → dispatch by RAX.

SFMASK clears IF on entry — no timer interrupts during syscall handling.

## Interrupts and Timer

- **PIC**: 8259A dual cascade, IRQ 0-15 remapped to vectors 32-47
- **PIT**: 8254 channel 0, ~1 kHz (divisor 1193), mode 2 rate generator
- **Timer ISR** (vector 32):
  - Kernel-mode: EOI + `iretq` (no preemption)
  - User-mode: save caller-saved regs → `timer_preempt` (tick++, EOI, mark Ready, yield) → restore → `iretq`
- **Interrupt gate**: clears IF automatically — no nested interrupts

## Scheduler

- **4 priority levels**: Critical, High, Normal, Low
- **Round-robin** within each level, tracked by `LAST_SCHEDULED` per level
- **Preemptive** via PIT timer + **cooperative** via `SYS_YIELD`
- **`context_switch`** (naked asm): pushes/pops callee-saved registers (RBX, RBP, R12-R15), swaps RSP
- **MAX_SHARDS**: 4, each with a 4 KiB kernel stack
