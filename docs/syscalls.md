# Syscall Reference

Every interaction between a shard and the supervisor goes through the `syscall` instruction. This page documents the calling convention, each syscall, and the rules the supervisor applies to user-provided values.

Constants live in `crates/coconut-shared/src/lib.rs` (Rust) and `include/coconut.h` (C). The dispatcher is `syscall_dispatch` in `crates/coconut-supervisor/src/syscall.rs`.

## Calling Convention

| Register | Role |
|----------|------|
| `rax` | Syscall number on entry, return value on exit |
| `rdi`, `rsi`, `rdx` | Arguments 0, 1, 2 |
| `rcx`, `r11` | Clobbered by `syscall`/`sysret` (user RIP and RFLAGS) |
| `rdi`, `rsi`, `rdx`, `r8`, `r9`, `r10` | Clobbered by the supervisor |
| `rbx`, `rbp`, `rsp`, `r12`-`r15` | Preserved |

All syscalls take at most three arguments. Errors are reported as `u64::MAX` (`COCONUT_ERROR` in C). The supervisor never panics on bad user input; a malformed argument returns `u64::MAX`, usually with a diagnostic line on the serial console.

Interrupts are masked for the duration of a syscall (`SFMASK` clears `IF`), so a syscall is never preempted. In particular, a single `SYS_SERIAL_WRITE` is never interleaved with other output.

## Buffer Validation

Syscalls that take a user pointer validate the whole range `[ptr, ptr + len)` before touching it. Lengths must be `1..=4096`, and `ptr + len` must not overflow.

| Region | Range | Readable by syscalls | Writable by syscalls |
|--------|-------|----------------------|----------------------|
| ELF image (text, rodata, data+bss) | `[0x1000, image_end)` | yes | data+bss segment only |
| mmap'd heap | `[data_start, data_end)` | yes | yes |
| Stack | `[0x7FF000, 0x800000)` | yes | yes |
| Anything else | | no | no |

The image segments are contiguous by construction (the supervisor asserts this at load time), so validation over the image range can never reach an unmapped page.

## Pledge

`SYS_GPU_PLEDGE` irreversibly restricts which syscalls a shard may make. Before the first pledge a shard is unrestricted. After it:

| Syscall | Allowed when |
|---------|-------------|
| `SYS_EXIT`, `SYS_YIELD`, `SYS_GPU_PLEDGE`, `SYS_GPU_UNVEIL`, `SYS_MMAP` | always |
| `SYS_SERIAL_WRITE` | `PLEDGE_SERIAL` (bit 0) is kept |
| `SYS_CHANNEL_SEND`, `SYS_CHANNEL_RECV` | `PLEDGE_CHANNEL` (bit 1) is kept |
| `SYS_GPU_DMA` | `PLEDGE_GPU_DMA` (bit 2) is kept |
| `SYS_PERF_COUNTER` | `PLEDGE_PERF` (bit 3) is kept |
| `SYS_CAP_*`, `SYS_FS_*`, unknown numbers | never |

A denied syscall returns `u64::MAX` and logs `Shard N: syscall X denied by pledge`.

## Syscalls

### Process

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 0 | `SYS_EXIT` | `code` | does not return |
| 62 | `SYS_YIELD` | | `0` |
| 71 | `SYS_PERF_COUNTER` | | current TSC value |

`SYS_EXIT` marks the shard exited and switches to the next ready shard. Shards killed by a fault handler take the same path with exit code `u64::MAX`.

### Console

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 1 | `SYS_SERIAL_WRITE` | `buf`, `len` | `0` |

`buf` must be readable (see [Buffer Validation](#buffer-validation)). `\n` is written as `\r\n`.

### Capabilities

Each shard has a table of 16 capability slots. A handle is an index into that table.

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 11 | `SYS_CAP_GRANT` | `handle`, `target_shard`, `rights` | handle in the target's table |
| 12 | `SYS_CAP_REVOKE` | `handle` | `0` |
| 13 | `SYS_CAP_RESTRICT` | `handle`, `rights` | `0` |
| 14 | `SYS_CAP_INSPECT` | `handle` | `(type << 48) \| (resource << 16) \| rights` |

- `SYS_CAP_GRANT` copies a capability into another shard's table. The copy's rights are the intersection of the source rights and `rights`, so a grant can never add rights. Channel capabilities require `RIGHT_CHANNEL_GRANT` on the source. The target must be running (ready, running, or blocked); grants to exited or unused slots are rejected.
- `SYS_CAP_RESTRICT` ANDs the rights with `rights`. Rights can only be removed.
- All four return `u64::MAX` for a handle `>= 16`, an empty slot, or a target shard ID `>= MAX_SHARDS`.

Capability types: `CAP_CHANNEL` (1), `CAP_SHARD` (2), `CAP_MEMORY` (3), `CAP_GPU_DMA` (4).
Rights: `RIGHT_CHANNEL_SEND` (1), `RIGHT_CHANNEL_RECV` (2), `RIGHT_CHANNEL_GRANT` (4), `RIGHT_GPU_DMA_WRITE` (1).

### Channels

Channels are single-buffered, bidirectional links between two shards, set up by the supervisor at boot. There are 4 channels with messages of up to 256 bytes.

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 21 | `SYS_CHANNEL_SEND` | `channel`, `buf`, `len` | `0` |
| 22 | `SYS_CHANNEL_RECV` | `channel`, `buf`, `max_len` | bytes received |

- Both require a `CAP_CHANNEL` capability for `channel` with the matching right (`SEND` or `RECV`).
- `len` must be `1..=256`; `buf` must be readable for send and writable for receive.
- Send copies the message into the channel buffer, replacing any unread message, and wakes the peer if it is blocked on this channel.
- Receive returns immediately if a message is waiting for the caller. Otherwise the caller blocks until the peer sends. A receiver whose peer has exited blocks forever.

### Filesystem

A read-only ext2 ramdisk generated at build time. The open-file table holds 16 entries shared by all shards; a descriptor is only usable by the shard that opened it.

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 30 | `SYS_FS_OPEN` | `path`, `path_len` | file descriptor |
| 31 | `SYS_FS_READ` | `fd`, `buf`, `max_len` | bytes read (`0` at end of file) |
| 32 | `SYS_FS_STAT` | `fd` | file size in bytes |
| 33 | `SYS_FS_CLOSE` | `fd` | `0` |

- `path_len` must be `1..=256` and `path` readable. Paths are absolute, e.g. `/hello.txt`. Only regular files can be opened; directories (`/`, `.`, `..`) are rejected.
- `SYS_FS_READ` validates `buf` as writable before reading.
- A shard should close every descriptor it opens. Any it leaves open are released when it exits or is killed.

### Memory

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 43 | `SYS_MMAP` | `va_start`, `num_pages` | `0` |

Maps zeroed, R+W+NX pages at `[va_start, va_start + num_pages * 4096)`.

- `va_start` must be page-aligned, and `num_pages` must be `1..=256`.
- The range must lie between the end of the ELF image and the stack (`0x7FF000`).
- After the first mapping, new mappings must be directly adjacent to the existing region (below or above it) and must not overlap it. This keeps the region gap-free for buffer validation.
- Each shard may own at most 256 frames in total. The budget also covers its PML4, image, stack, kernel stack, and config pages; intermediate page tables are not counted.

### GPU

These syscalls are only meaningful for GPU HAL shards, which own a VRAM partition.

| # | Name | Arguments | Returns |
|---|------|-----------|---------|
| 40 | `SYS_GPU_DMA` | `target_partition`, `src_offset`, `(dst_offset << 32) \| len` | `0` |
| 41 | `SYS_GPU_PLEDGE` | `mask` | `0` |
| 42 | `SYS_GPU_UNVEIL` | `offset`, `size` | `0` |

- `SYS_GPU_DMA` copies `len` bytes from the caller's partition to another partition. It requires `CAP_GPU_DMA` with `RIGHT_GPU_DMA_WRITE` for the target partition, both ranges must fit their partitions, and both must lie inside each shard's unveiled range if one is set.
- `SYS_GPU_PLEDGE` ANDs the shard's pledge mask with `mask` (see [Pledge](#pledge)).
- `SYS_GPU_UNVEIL` locks the VRAM range the shard may use as a DMA source or destination. It can be called once; the range must fit in the caller's partition.

### Unknown Numbers

Any other syscall number returns `u64::MAX`. Syscall numbers are compared as full 64-bit values.
