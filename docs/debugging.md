# Debugging coconutOS

## Serial Output

All kernel output goes to COM1 (0x3F8, 115200 8N1). The `qemu-run.sh` script maps serial to stdio (`-serial stdio`), so output appears in your terminal.

Messages are prefixed by subsystem:

| Prefix | Source |
|--------|--------|
| `[BOOT]` | Bootloader (`coconut-boot`) |
| (none) | Supervisor init (`coconut-supervisor`) |

Shard serial writes (via `SYS_SERIAL_WRITE`) appear without a prefix. The inference shard prefixes its own messages with `llama-inference:`.

## GDB Remote Debugging

### Start QEMU with GDB stub

```bash
# Pass -s -S to QEMU via the run script
./scripts/qemu-run.sh -s -S
```

- `-s`: Open GDB server on `localhost:1234`
- `-S`: Freeze CPU at startup (wait for GDB `continue`)

### Connect GDB

```bash
# Use rust-gdb for pretty-printing Rust types
rust-gdb target/x86_64-unknown-none/release/coconut-supervisor \
    -ex "target remote :1234"
```

Useful GDB commands:

```gdb
# Set breakpoint at supervisor entry
break supervisor_main

# Continue past UEFI boot
continue

# Print page table base
info registers cr3

# Examine memory at physical address (via HHDM)
x/16gx 0xFFFF800000400000

# Step through assembly
si

# Show current instruction
x/i $rip
```

## Common Faults

### Triple Fault (QEMU resets or hangs)

Usually means the CPU took a fault while trying to deliver another fault (double fault), then faulted again. Common causes:

- **Bad page tables**: The trampoline built incorrect mappings. Check that PML4, PDPT, and PD entries are present and correctly aligned.
- **Stack not mapped**: The kernel stack must be accessible at the current RSP. If RSP is a physical address but identity mapping was removed, the CPU faults immediately.
- **IDT not loaded**: If `lidt` hasn't run, any interrupt triggers a triple fault.

Diagnosis: Add serial prints at each init stage to find the last successful step.

### General Protection Fault (#GP, vector 13)

- **Segment selector wrong**: GDT index or RPL mismatch. Check that `sysretq` uses the correct CS/SS selectors (STAR MSR bits [63:48]).
- **Canonical address violation**: Jumping to a non-canonical address (bits 47-63 not all same).
- **Privileged instruction in ring 3**: User code executing `cli`, `hlt`, `in`/`out`, etc.
- **CR4.TSD violation**: User-mode `rdtsc`/`rdtscp` causes #GP when CR4.TSD is set (this is intentional — prevents timing side-channels).

User-mode #GP faults are caught by the GPF handler, which kills the offending shard via `handle_sys_exit(u64::MAX)` rather than panicking the supervisor.

### Page Fault (#PF, vector 14)

The fault handler prints CR2 (faulting address) and the error code. Error code bits:

| Bit | Meaning when set |
|-----|-----------------|
| 0 | Page was present (protection violation vs. not-present) |
| 1 | Write access |
| 2 | User-mode access |
| 3 | Reserved bit set in page table entry |
| 4 | Instruction fetch (NX violation) |

Common causes:
- Shard accessing unmapped memory (only code, mmap'd data, and stack pages are mapped)
- Writing to a read-only page (code pages at `0x1000+` are R+X)
- Executing from a non-executable page (stack at `0x7FF000` is R+W+NX, data at `0x100000+` is R+W+NX)
- **BSS in code region**: C shards compiled as flat binaries have no separate BSS segment. Mutable globals land in the code region (R+X), causing page faults on write. Move all mutable state to stack or mmap'd heap.

### Float Math Corruption After Preemption

If a shard's float computations produce wrong results after being preempted by the timer, check:

- **FXSAVE/FXRSTOR in timer ISR**: The user-mode timer ISR path must save and restore the 512-byte FPU/SSE state around the call to `timer_preempt`.
- **CR4.OSFXSR**: Bit 9 must be set for FXSAVE/FXRSTOR to work.
- **Supervisor SSE disabled**: The supervisor must be compiled with `-C target-feature=-sse,-sse2` to prevent Rust-generated SSE instructions from clobbering user XMM registers during syscall handling.

## Disassembly

### Supervisor binary

```bash
# Full disassembly
mise run objdump

# Or directly:
rust-objdump -d --no-show-raw-insn \
    target/x86_64-unknown-none/release/coconut-supervisor

# Specific section
rust-objdump -d --section=.text.boot \
    target/x86_64-unknown-none/release/coconut-supervisor
```

### ELF headers and sections

```bash
rust-readobj --file-headers --sections --segments \
    target/x86_64-unknown-none/release/coconut-supervisor
```

### Bootloader (PE32+)

```bash
rust-objdump -d --no-show-raw-insn \
    target/x86_64-unknown-uefi/release/coconut-boot.efi
```

### Shard binaries

C shard flat binaries can be disassembled with:

```bash
llvm-objdump -d -m x86-64 --no-show-raw-insn shard.bin
```

## Memory Layout Quick Reference

### Physical Memory

| Address | Size | Contents |
|---------|------|----------|
| `0x000000` | — | Low memory (reserved) |
| `0x200000` | ~256 KiB | Supervisor ELF (loaded by bootloader) |
| `0x300000` | 4 KiB | Temporary boot stack |
| `0x400000` | 28 KiB (7 pages) | Boot page tables (PML4, PDPTs, PDs) |
| `0x800000+` | varies | PMM-managed free memory |

### Virtual Memory (after higher-half transition)

| Virtual Address | Maps To | Purpose |
|----------------|---------|---------|
| `0x0000000000001000+` | Shard code frames | User code (R+X, multi-page) |
| `0x0000000000100000+` | Shard data frames | mmap'd heap (R+W+NX) |
| `0x00000000007FF000` | Shard stack frame | User stack (R+W+NX) |
| `0x0000000000800000+` | GPU BAR frames | GPU VRAM/MMIO (HAL shards, ASLR'd) |
| `0xFFFF800000000000` + phys | Physical address | HHDM (runtime phys→virt) |
| `0xFFFFFFFF80200000` | `0x200000` | Supervisor code/data |
| `0xFFFFFFFFC0000000+` | Device BARs | MMIO mappings (PCD+PWT+NX) |

### Page Table Indices

| PML4 Index | Virtual Base | Purpose |
|-----------|-------------|---------|
| 0 | `0x0000000000000000` | Identity map (boot only) / shard user pages |
| 256 | `0xFFFF800000000000` | Higher-Half Direct Map (HHDM) |
| 511 | `0xFFFFFFFF80000000` | Kernel text/data/bss/stack + MMIO |
