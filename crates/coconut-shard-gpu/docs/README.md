# coconut-shard-gpu

GPU HAL shard binary for coconutOS. Validates GPU device access, runs a compute dispatch test, and exercises inter-partition DMA.

## Purpose

This is a user-mode shard that runs on each GPU partition. The supervisor creates one instance per partition (currently 2), each mapped to its own VRAM and MMIO regions at ASLR'd virtual addresses. The shard exercises the full GPU subsystem: config page reading, VRAM allocation, command ring dispatch, matrix multiply, memory zeroing, and inter-partition DMA.

## Execution Flow

1. **Read config page** — `GpuConfig::read()` at VA `0x4000`, validates magic
2. **pledge/unveil** — restrict to `SERIAL | CHANNEL | GPU_DMA`, lock VRAM range
3. **VRAM test** — write/readback `0xDEADBEEF` to verify VRAM mapping
4. **MMIO test** — read VBE ID register at MMIO+0x500 (QEMU std VGA)
5. **Init allocator** — `VramAllocator::init()` at VRAM base
6. **Alloc resources** — command ring (4 KiB) + matrices A, B, C (64 bytes each)
7. **Write matrices** — A = [1..16], B = 2*I₄ (identity scaled by 2)
8. **Dispatch matmul** — submit via command ring, compute C = A × B, mark complete
9. **Verify results** — check C[i] == 2 * A[i] for all 16 elements
10. **Free + zero** — free all allocations in reverse order, verify VRAM zeroed
11. **DMA test** — partition 0 sends 64 bytes to partition 1 via `SYS_GPU_DMA`, partition 1 blocks on IPC channel then verifies received data

## DMA Protocol

- **Partition 0 (sender):** writes [1..16] at VRAM+0x100000, issues `SYS_GPU_DMA` to copy to partition 1, signals completion via `SYS_CHANNEL_SEND`
- **Partition 1 (receiver):** blocks on `SYS_CHANNEL_RECV`, then verifies [1..16] at VRAM+0x100000

## Expected Serial Output

```
GPU mem: freed+zeroed, compute ok
GPU DMA: sent 64 bytes to partition 1
GPU DMA: recv ok, verified
```

## Module Overview

| File | Purpose |
|------|---------|
| `src/main.rs` | `main()` entry, compute test, DMA sender/receiver |

## Dependencies

- `coconut-rt` (workspace — entry point, syscall wrappers, GPU primitives)
- `coconut-shared` (workspace — pledge constants)

## Build Target

```
targets/x86_64-coconut-shard.json
```

Built as part of the supervisor's `build.rs`: cargo builds the crate, `llvm-objcopy -O binary` produces a flat binary, which is embedded via `include_bytes!` in the supervisor.
