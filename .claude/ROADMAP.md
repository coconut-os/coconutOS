# coconutOS Roadmap

## CPU-Only Shard Model — Complete

Functional microkernel with CPU-only shards, no GPU support.

- [x] **0.1** Boot on x86-64 (QEMU/UEFI), physical memory manager, serial console
- [x] **0.2** Higher-half kernel, shard creation and destruction
- [x] **0.3** IPC channels between shards (synchronous message passing)
- [x] **0.4** Preemptive round-robin scheduler (4 priority levels, PIT timer)
- [x] **0.5** Capability system (grant, revoke, restrict, inspect)
- [x] **0.6** Minimal read-only filesystem (ext2 ramdisk, build-time generated)

## GPU Bring-Up — AMD — Next

GPU HAL shard for AMD RDNA3/CDNA3, basic compute dispatch.

- [x] **1.1** GPU PCIe enumeration and IOMMU domain setup
- [x] **1.2** GPU HAL shard: device init, memory alloc, command queue
- [x] **1.3** Basic compute dispatch (4×4 matrix multiply via command ring)
- [x] **1.4** GPU memory management with typed allocations
- [x] **1.5** VRAM zeroing on free, W^X enforcement
- [x] **1.6** Performance baseline: compare GPU compute throughput vs. Linux/ROCm

## Multi-Shard GPU Isolation — Planned

Multiple inference shards with strong isolation on a single GPU.

- [x] **2.1** GPU partitioning (CU slicing, VRAM carving)
- [x] **2.2** Multiple GPU HAL shard instances (one per partition)
- [x] **2.3** Inter-shard GPU DMA (pipeline parallelism)
- [x] **2.4** `pledge_gpu` / `unveil_vram` enforcement
- [x] **2.5** GPU ASLR
- [x] **2.6** Side-channel isolation testing and hardening

## Inference Stack — Planned

End-to-end LLM inference on coconutOS.

- [x] **3.1** Inference runtime library (Rust API)
- [x] **3.2** C ABI / FFI layer
- [x] **3.3** Port llama.cpp as proof-of-concept inference shard
- [x] **3.4** Inference pipeline protocol (multi-shard pipeline parallelism)
- [x] **3.5** coconut-trace, coconut-prof tooling
- [ ] **3.6** Benchmark: Llama 70B inference latency vs. Linux/ROCm baseline

## Hardening & Multi-Vendor — Planned

Production hardening, additional GPU vendor support.

- [ ] **4.1** Security audit of supervisor (external)
- [ ] **4.2** Fuzzing campaign (syzkaller-style for supervisor syscalls)
- [ ] **4.3** NVIDIA GPU HAL shard (Hopper/Blackwell)
- [ ] **4.4** Apple GPU HAL shard (M-series, ARM64 port)
- [ ] **4.5** Network shard with RDMA/GPU-Direct support
- [ ] **4.6** Formal verification of supervisor capability system (Verus or similar)
