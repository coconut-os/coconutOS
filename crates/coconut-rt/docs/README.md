# coconut-rt

Runtime library for coconutOS Rust shards. Provides the entry point, panic handler, syscall wrappers, serial I/O macros, and GPU primitives.

## Purpose

Every Rust shard links against `coconut-rt` to get a working `_start` entry stub, a panic handler that prints to serial and exits, and safe wrappers around coconutOS syscalls. The crate is `#![no_std]` and depends only on `coconut-shared`.

## Module Overview

| File | Purpose |
|------|---------|
| `src/lib.rs` | Entry stub (`_start` in `.text.entry`), panic handler |
| `src/sys.rs` | Syscall wrappers — one inline asm function per coconutOS syscall |
| `src/io.rs` | `SerialWriter` (`fmt::Write` impl), `print!` and `println!` macros |
| `src/gpu.rs` | GPU config page, VRAM allocator, command ring, matmul compute |

## Entry Point

The `_start` stub (placed in `.text.entry` so the linker script puts it first at VA `0x1000`):
1. Sets RSP to `0x800000` (top of stack page)
2. Calls the shard's `main()` function
3. Issues `SYS_EXIT(0)` if main returns

Shards must define `#[no_mangle] pub extern "C" fn main()`.

## Syscall Wrappers (`sys`)

| Function | Syscall | Description |
|----------|---------|-------------|
| `exit(code)` | `SYS_EXIT` | Terminate shard (diverges) |
| `serial_write(buf)` | `SYS_SERIAL_WRITE` | Write bytes to serial console |
| `gpu_pledge(mask)` | `SYS_GPU_PLEDGE` | Monotonic syscall restriction |
| `gpu_unveil(offset, size)` | `SYS_GPU_UNVEIL` | Lock VRAM range for DMA |
| `gpu_dma(target, src_off, packed)` | `SYS_GPU_DMA` | Inter-partition VRAM copy |
| `channel_send(ch, buf)` | `SYS_CHANNEL_SEND` | Send IPC message |
| `channel_recv(ch, buf)` | `SYS_CHANNEL_RECV` | Receive IPC message (blocking) |
| `yield_now()` | `SYS_YIELD` | Cooperative yield |

All wrappers correctly declare clobbered registers — the kernel's `syscall_entry` only preserves callee-saved regs (RBX, RBP, R12-R15).

## GPU Primitives (`gpu`)

**`GpuConfig`** — reads the kernel-provided config page at VA `0x4000`:
- `partition_id`, `vram_size`, `cu_count`, `vram_vaddr`, `mmio_vaddr`
- Validates magic `0x47504346` ("GPCF")

**`VramAllocator`** — bump allocator for VRAM with typed entries:
- Header at VRAM+0x00: magic, alloc_count, next_offset, total_size
- Table at VRAM+0x10: 16-byte entries (type, offset, size, reserved)
- 64-byte alignment (GPU cache line), max 255 entries
- `free()` zeroes the region before marking the entry

**`CommandRing`** — VRAM-based dispatch queue:
- Header: magic ("RING"), write_ptr, read_ptr, ring_size
- `submit_matmul()` writes a dispatch entry, `complete()` marks it done

**`matmul_4x4(a, b, c)`** — 4x4 u32 matrix multiply using volatile VRAM access.

## Serial I/O (`io`)

`SerialWriter` implements `core::fmt::Write`, used by the `print!` and `println!` macros:

```rust
coconut_rt::println!("Hello from shard!");
coconut_rt::println!("partition {}: {} CUs", config.partition_id, config.cu_count);
```

## Dependencies

- `coconut-shared` (workspace — syscall constants, pledge bits)

## Build Target

```
targets/x86_64-coconut-shard.json
```

Compiled with `-Zjson-target-spec`, then `llvm-objcopy -O binary` to produce a flat binary embedded in the supervisor via `include_bytes!`.
