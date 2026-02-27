# coconutOS Architecture

> **Version:** 0.3.4
> **Date:** 2026-02-27
> **Status:** GPU isolation complete, inference stack near-complete (profiling done, benchmark remaining)

---

## Table of Contents

1. [Introduction & Motivation](#1-introduction--motivation)
2. [System Architecture Overview](#2-system-architecture-overview)
3. [Microkernel Shard Architecture](#3-microkernel-shard-architecture)
4. [GPU Hardware Abstraction Layer](#4-gpu-hardware-abstraction-layer)
5. [Security Model](#5-security-model)
6. [Boot Process](#6-boot-process)
7. [Scheduler Design](#7-scheduler-design)
8. [Memory Management](#8-memory-management)
9. [Inter-Process Communication (IPC)](#9-inter-process-communication-ipc)
10. [Networking](#10-networking)
11. [Userland & Programming Model](#11-userland--programming-model)
12. [Filesystem & Storage](#12-filesystem--storage)
13. [Development Roadmap](#13-development-roadmap)
14. [Open Questions & Risks](#14-open-questions--risks)
15. [Appendices](#15-appendices)

---

## 1. Introduction & Motivation

### 1.1 Problem Statement

GPU isolation in modern operating systems is fundamentally bolted-on. Linux treats GPUs as monolithic devices behind a single kernel driver, with isolation enforced through ad-hoc mechanisms (cgroups, MIG, SR-IOV) that were never designed for security-critical AI inference workloads. The result:

- **No real isolation.** A compromised inference job can read VRAM belonging to another tenant. GPU memory is not zeroed between allocations by default. Side-channel attacks on GPU caches are practical and largely unmitigated.
- **Massive attack surface.** GPU kernel drivers (e.g., `amdgpu` at ~500K LoC, NVIDIA's proprietary blob at ~30M LoC) run in ring 0. A single driver bug yields full kernel compromise.
- **Architectural mismatch.** Monolithic kernels cannot enforce per-workload GPU access policies. There is no `pledge(2)` for GPU compute, no `unveil(2)` for VRAM regions.

### 1.2 Why a New OS

Patching Linux is insufficient because the isolation boundary is in the wrong place. Linux's driver model assumes a trusted kernel with exclusive GPU access. Moving the driver to userspace (as in a microkernel) changes the trust model entirely: the supervisor never touches GPU registers directly, and each GPU partition is mediated by a shard with its own address space and capabilities.

OpenBSD demonstrates that a security-first UNIX can be practical. coconutOS takes OpenBSD's principles — small attack surface, pledge/unveil, W^X, ASLR, minimal defaults — and applies them to a GPU-native microkernel.

### 1.3 Design Principles

| # | Principle | Implication |
|---|-----------|-------------|
| 1 | **Isolation by default** | Every inference workload runs in its own shard with separate address space, VRAM partition, and capability set. No sharing unless explicitly granted. |
| 2 | **Small trusted computing base** | The supervisor (microkernel) targets <10K LoC. GPU drivers run in user-mode shards. |
| 3 | **GPU as a first-class resource** | The scheduler, memory manager, and IPC system are all GPU-aware from day one — not retrofitted. |
| 4 | **Rust everywhere** | All kernel and userland code is Rust (with `unsafe` blocks auditable and minimized). No C in the TCB. |
| 5 | **OpenBSD philosophy** | Secure defaults, minimal surface, correct over fast, audit everything. |
| 6 | **Formal verifiability** | The supervisor's critical paths (capability checks, IPC dispatch, shard lifecycle) are designed to be amenable to formal verification. |

### 1.4 Novelty Claim

coconutOS is the first operating system designed from scratch where GPU compute isolation is a kernel-level primitive rather than a userspace afterthought. The combination of microkernel shards, GPU-native scheduling, and OpenBSD-style security policies is unique.

---

## 2. System Architecture Overview

### 2.1 Layered Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     Applications                         │
│          (inference clients, management tools)           │
├─────────────────────────────────────────────────────────┤
│                    Service Shards                         │
│    (network stack, filesystem, logging, metrics)         │
├─────────────────────────────────────────────────────────┤
│                   Inference Shards                        │
│  (model runtime, GPU compute, per-workload isolation)    │
├─────────────────────────────────────────────────────────┤
│                  GPU HAL Shards                           │
│    (user-mode GPU drivers, one per GPU/partition)        │
├─────────────────────────────────────────────────────────┤
│               ┌───────────────────┐                      │
│               │    Supervisor     │                      │
│               │   (<10K LoC)      │                      │
│               │                   │                      │
│               │  - Capability mgr │                      │
│               │  - IPC dispatch   │                      │
│               │  - Shard lifecycle│                      │
│               │  - Scheduler core │                      │
│               │  - Memory regions │                      │
│               └───────────────────┘                      │
├─────────────────────────────────────────────────────────┤
│                      Hardware                            │
│   CPU cores │ RAM │ GPUs │ NVMe │ NIC │ IOMMU │ TPM    │
└─────────────────────────────────────────────────────────┘
```

### 2.2 Component Inventory

| Component | Trust Level | LoC Target | Language | Runs In |
|-----------|-------------|------------|----------|---------|
| Supervisor | TCB | <10K | Rust (`no_std`) | Ring 0 / EL1 |
| GPU HAL shard | Semi-trusted | ~50K per vendor | Rust | User-mode shard |
| Network shard | Untrusted | ~20K | Rust | User-mode shard |
| Filesystem shard | Untrusted | ~10K | Rust | User-mode shard |
| Inference shard | Untrusted | Varies | Rust + C FFI | User-mode shard |
| Boot loader | TCB (transient) | ~5K | Rust | Firmware/EL2 |

### 2.3 Threat Model

**In scope:**
- Malicious inference workloads attempting to escape their shard
- Side-channel attacks between GPU shards (timing, cache, power)
- Compromised GPU drivers attempting kernel escalation
- DMA attacks from malicious/buggy peripherals
- Network-based attacks on exposed inference endpoints

**Out of scope (v1):**
- Physical access attacks (cold boot, bus probing)
- Supply-chain attacks on GPU firmware/microcode
- Denial-of-service via legitimate resource exhaustion (handled by quotas, not security boundaries)

**Trust boundaries:**
1. Supervisor ↔ any shard (capability-mediated syscall interface)
2. Shard ↔ shard (IPC channels, no direct memory access)
3. GPU HAL shard ↔ GPU hardware (IOMMU-enforced DMA regions)
4. Network shard ↔ external network (packet filtering, TLS termination)

---

## 3. Microkernel Shard Architecture

This is the central abstraction in coconutOS. A **shard** is the unit of isolation, scheduling, and resource management.

### 3.1 What Is a Shard?

A shard is a lightweight isolated execution environment that combines:
- An independent virtual address space (CPU)
- Zero or more GPU memory partitions
- A capability set defining permitted operations
- One or more threads of execution
- A set of IPC channel endpoints

Shards are **not** containers (no shared kernel state), **not** VMs (no hardware virtualization overhead), and **not** processes (GPU resources are first-class, not bolted on).

```
┌──────────────── Shard ─────────────────┐
│                                         │
│  ┌─────────┐  ┌─────────┐  ┌────────┐ │
│  │ Thread 0│  │ Thread 1│  │Thread N│ │
│  └────┬────┘  └────┬────┘  └───┬────┘ │
│       │            │            │       │
│  ┌────┴────────────┴────────────┴────┐ │
│  │        Virtual Address Space       │ │
│  │  ┌──────┐ ┌──────┐ ┌───────────┐ │ │
│  │  │ Code │ │ Heap │ │ GPU MMIO  │ │ │
│  │  └──────┘ └──────┘ └───────────┘ │ │
│  └───────────────────────────────────┘ │
│                                         │
│  ┌───────────────────────────────────┐ │
│  │        GPU Partition               │ │
│  │  ┌─────────┐ ┌─────────────────┐ │ │
│  │  │ VRAM    │ │ Command Queues  │ │ │
│  │  │ Region  │ │ (compute/copy)  │ │ │
│  │  └─────────┘ └─────────────────┘ │ │
│  └───────────────────────────────────┘ │
│                                         │
│  ┌───────────────────────────────────┐ │
│  │ Capabilities: {gpu.compute,       │ │
│  │   ipc.channel:5, net.none,        │ │
│  │   mem.alloc:4GiB, vram.alloc:8GiB}│ │
│  └───────────────────────────────────┘ │
└─────────────────────────────────────────┘
```

### 3.2 Shard Lifecycle

```
          create()
    ┌────────┐
    │        ▼
    │   ┌─────────┐    boot()    ┌─────────┐
    │   │ Created  │────────────▶│ Booting │
    │   └─────────┘              └────┬────┘
    │                                  │
    │                            ready │
    │                                  ▼
    │                            ┌─────────┐   scale()   ┌─────────┐
    │                            │ Running │◀───────────▶│ Scaling │
    │                            └────┬────┘             └─────────┘
    │                                  │
    │              destroy() or fault  │
    │                                  ▼
    │                            ┌──────────┐
    └────────────────────────────│ Destroyed│
                                 └──────────┘
```

**States (implemented):**

| State | Description |
|-------|-------------|
| **Free** | Shard slot unoccupied. |
| **Ready** | Runnable, waiting for scheduler to select it. |
| **Running** | Currently executing on the CPU. |
| **Blocked** | Waiting on IPC channel recv. |
| **Exited** | Shard called `SYS_EXIT`, awaiting cleanup. |
| **Destroyed** | All resources reclaimed — frames freed, page tables torn down, capabilities cleared. |

**Lifecycle operations (implemented):**

- `shard::create(code, name, priority)` — Allocate page tables, map code + stack, prepare kernel context.
- `scheduler::run_loop()` — Pick Ready shards, context-switch, handle exit/block.
- `shard::destroy(id)` — Free all frames, tear down page tables, clear capabilities, zero memory.

### 3.3 The Supervisor

The supervisor is the only code running in ring 0 (or EL1 on ARM). It is intentionally minimal:

**Responsibilities:**
- Shard lifecycle management (create, boot, destroy)
- Capability creation, transfer, and revocation
- IPC message dispatch (fast-path)
- CPU scheduling (shard-level time slicing)
- Physical memory region management
- IOMMU configuration
- Interrupt routing to shards
- Timer management

**Non-responsibilities (delegated to shards):**
- GPU compute logic (HAL shards)
- Network stack (planned — network shard)
- Inference runtime (inference shards)
- Logging, metrics, tracing (planned — service shards)

**Code budget:** The supervisor targets <10,000 lines of Rust (`no_std`, `no_alloc` in critical paths). This is comparable to seL4's ~10K LoC verified kernel. The small size enables:
- Complete manual audit
- Fuzzing of all syscall paths
- Eventual formal verification of critical properties (capability safety, IPC correctness, memory isolation)

### 3.4 Inter-Shard IPC

See [Section 9](#9-inter-process-communication-ipc) for full IPC details. Summary:

- **Channels:** Bidirectional, capability-mediated, supervisor-dispatched message passing.
- **Shared memory:** Opt-in shared regions between cooperating shards, created via supervisor grant.
- **GPU DMA:** Direct GPU-to-GPU memory transfer between shards, mediated by IOMMU rules.

### 3.5 Comparison: Shards vs. Alternatives

| Property | Linux Container | VM (KVM) | seL4 Process | Fuchsia Job | **coconutOS Shard** |
|----------|----------------|----------|-------------|-------------|-------------------|
| Isolation mechanism | Namespaces + cgroups | Hardware virtualization | Capability-based | Capability-based | Capability-based |
| GPU isolation | None (shared driver) | PCI passthrough (1 VM = 1 GPU) | Not GPU-aware | Not GPU-aware | **Native GPU partitions** |
| GPU memory zeroing | No | On VM destroy only | N/A | N/A | **On every free** |
| Overhead | Low | High (trap-and-emulate) | Low | Low | **Low** |
| TCB size | ~28M LoC (kernel) | ~28M LoC + QEMU | ~10K LoC | ~200K LoC (Zircon) | **<10K LoC** |
| GPU scheduling | Kernel driver | Hypervisor passthrough | N/A | N/A | **Integrated 3-level** |
| Formal verification | No | No | Yes (functional correctness) | No | **Planned (critical paths)** |
| Live GPU rescaling | No | No | N/A | N/A | **Yes (shard_scale)** |

### 3.6 Formal Properties

The following properties are targets for formal verification of the supervisor:

1. **Capability safety:** A shard cannot invoke an operation without holding the corresponding capability. Capabilities cannot be forged.
2. **Spatial isolation:** A shard cannot read or write memory (CPU or GPU) outside its assigned regions.
3. **Temporal isolation:** A destroyed shard's memory (CPU and GPU) is zeroed before reassignment.
4. **IPC integrity:** Messages are delivered exactly once, to the correct destination, without modification.
5. **Liveness:** The supervisor's IPC dispatch and scheduling loops are guaranteed to make progress (no unbounded blocking in the supervisor).

---

## 4. GPU Hardware Abstraction Layer

### 4.1 Five-Layer HAL Stack

```
┌──────────────────────────────────────────┐
│  Layer 5: Compute Abstraction            │
│  (GpuCompute trait — dispatch, sync)     │
├──────────────────────────────────────────┤
│  Layer 4: Command Submission             │
│  (GpuCommandQueue — ring buffers, fences)│
├──────────────────────────────────────────┤
│  Layer 3: Memory Management              │
│  (GpuMemory — alloc, map, DMA)           │
├──────────────────────────────────────────┤
│  Layer 2: Partitioning                   │
│  (GpuPartition — CU slicing, VRAM carve) │
├──────────────────────────────────────────┤
│  Layer 1: Device Abstraction             │
│  (GpuDevice — discovery, reset, power)   │
└──────────────────────────────────────────┘
```

Each layer is defined as a Rust trait. Vendor backends implement these traits in user-mode GPU HAL shards.

### 4.2 Core Rust Traits

```rust
/// Layer 1: Physical GPU device.
pub trait GpuDevice: Send + Sync {
    fn device_id(&self) -> DeviceId;
    fn vendor(&self) -> GpuVendor;
    fn capabilities(&self) -> DeviceCapabilities;
    fn reset(&mut self) -> Result<(), GpuError>;
    fn power_state(&self) -> PowerState;
    fn set_power_state(&mut self, state: PowerState) -> Result<(), GpuError>;
    fn thermal_info(&self) -> ThermalInfo;
}

/// Layer 2: Logical partition of a GPU.
pub trait GpuPartition: Send + Sync {
    fn partition_id(&self) -> PartitionId;
    fn parent_device(&self) -> DeviceId;
    fn compute_units(&self) -> Range<u32>;
    fn vram_region(&self) -> MemoryRegion;
    fn resize(&mut self, new_cus: u32, new_vram: usize) -> Result<(), GpuError>;
    fn isolate(&mut self) -> Result<(), GpuError>;  // Enforce hard isolation fences
}

/// Layer 3: GPU memory operations.
pub trait GpuMemory: Send + Sync {
    fn allocate(&mut self, desc: GpuAllocDesc) -> Result<GpuAllocation, GpuError>;
    fn free(&mut self, alloc: GpuAllocation) -> Result<(), GpuError>;
    fn map_to_cpu(&mut self, alloc: &GpuAllocation) -> Result<*mut u8, GpuError>;
    fn unmap_from_cpu(&mut self, alloc: &GpuAllocation) -> Result<(), GpuError>;
    fn zero(&mut self, alloc: &GpuAllocation) -> Result<(), GpuError>;
    fn usage(&self) -> MemoryUsage;
}

/// Layer 4: Command queue for GPU work submission.
pub trait GpuCommandQueue: Send + Sync {
    fn submit(&mut self, commands: &[GpuCommand]) -> Result<FenceId, GpuError>;
    fn wait_fence(&self, fence: FenceId, timeout: Duration) -> Result<(), GpuError>;
    fn poll_fence(&self, fence: FenceId) -> FenceStatus;
    fn drain(&mut self) -> Result<(), GpuError>;
}

/// Layer 5: High-level compute dispatch.
pub trait GpuCompute: Send + Sync {
    fn load_shader(&mut self, binary: &[u8]) -> Result<ShaderId, GpuError>;
    fn unload_shader(&mut self, shader: ShaderId) -> Result<(), GpuError>;
    fn dispatch(
        &mut self,
        shader: ShaderId,
        args: &GpuDispatchArgs,
    ) -> Result<FenceId, GpuError>;
    fn dispatch_indirect(
        &mut self,
        shader: ShaderId,
        args_buffer: &GpuAllocation,
    ) -> Result<FenceId, GpuError>;
}

/// GPU-to-GPU and GPU-to-CPU DMA operations.
pub trait GpuDma: Send + Sync {
    fn copy_device_to_device(
        &mut self,
        src: &GpuAllocation,
        dst: &GpuAllocation,
        size: usize,
    ) -> Result<FenceId, GpuError>;
    fn copy_host_to_device(
        &mut self,
        src: *const u8,
        dst: &GpuAllocation,
        size: usize,
    ) -> Result<FenceId, GpuError>;
    fn copy_device_to_host(
        &mut self,
        src: &GpuAllocation,
        dst: *mut u8,
        size: usize,
    ) -> Result<FenceId, GpuError>;
    fn peer_copy(
        &mut self,
        src_partition: PartitionId,
        src_alloc: &GpuAllocation,
        dst_partition: PartitionId,
        dst_alloc: &GpuAllocation,
        size: usize,
    ) -> Result<FenceId, GpuError>;
}
```

### 4.3 Type Definitions

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Amd,
    Nvidia,
    Intel,
    Apple,
}

#[derive(Debug, Clone)]
pub struct GpuAllocDesc {
    pub size: usize,
    pub alignment: usize,
    pub usage: GpuMemoryUsage,
    pub zero_on_alloc: bool,  // Always true for inference shards
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuMemoryUsage {
    Weights,       // Read-only after load, large, contiguous
    Activations,   // Read-write, ephemeral per inference
    KvCache,       // Read-write, grows with sequence length
    Scratch,       // Temporary computation buffers
    CommandBuffer, // Ring buffer for command queues
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryRegion {
    pub base: u64,     // Physical or IOMMU-virtual address
    pub size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceStatus {
    Pending,
    Signaled,
    Error(GpuError),
}
```

### 4.4 Vendor Backend Strategy

| Priority | Vendor | Hardware Target | Approach |
|----------|--------|-----------------|----------|
| **Phase 1** | AMD | RDNA 3 / CDNA 3 (MI300) | Open-source register specs + Mesa/AMDGPU reference. User-mode driver shard. |
| Phase 3 | Intel | Arc (Xe) | Open-source i915 reference. Lower priority. |
| Phase 4 | NVIDIA | Hopper / Blackwell | Reverse-engineered nouveau-style or future open-source driver. Highest complexity. |
| Phase 4 | Apple | M-series (Apple GPU) | Asahi Linux reverse-engineering reference. ARM-only. |

AMD is the initial target because:
1. Open-source register documentation exists
2. AMDGPU kernel driver and Mesa userspace are fully open-source references
3. CDNA/MI300 is the primary non-NVIDIA AI accelerator
4. AMD GPUs support hardware-level compute partitioning

### 4.5 GPU Memory Model

coconutOS enforces a typed GPU memory model. Every VRAM allocation has a declared `GpuMemoryUsage` type that determines:

| Usage Type | Permissions | Zeroing | Lifetime |
|------------|-------------|---------|----------|
| `Weights` | Read-only after load | On free | Shard lifetime |
| `Activations` | Read-write | On alloc + free | Per-inference |
| `KvCache` | Read-write | On free | Per-session |
| `Scratch` | Read-write | On free | Per-dispatch |
| `CommandBuffer` | Write (CPU) → Read (GPU) | On free | Queue lifetime |

The `Weights` region is made read-only after initial DMA load, preventing runtime corruption. `Activations` are zeroed on allocation to prevent cross-inference data leaks.

### 4.6 The Driver Complexity Problem

GPU drivers are inherently complex. The AMD AMDGPU kernel driver alone is ~500K LoC. coconutOS mitigates this by:

1. **User-mode execution.** GPU driver bugs crash the HAL shard, not the supervisor. The shard can be restarted.
2. **IOMMU containment.** A buggy driver cannot DMA outside its assigned regions.
3. **Minimal driver scope.** coconutOS drivers only need to support compute (no display, no video decode, no OpenGL). This eliminates ~60% of driver complexity.
4. **Incremental bring-up.** Start with the minimum set of registers needed for compute dispatch, memory allocation, and power management. No attempt to support the full GPU feature set.

Estimated reduced driver LoC per vendor: ~50K (compute-only) vs. ~500K (full driver).

---

## 5. Security Model

### 5.1 Capability-Based Access Control

Every resource in coconutOS is accessed through capabilities — unforgeable tokens that encode both the resource identity and the permitted operations.

```
Per-shard capability table (kernel-side, 16 entries per shard):
┌──────────┬──────────┬────────────┐
│ Type (8) │ ID (16)  │ Rights (16)│
└──────────┴──────────┴────────────┘
```

**Capability types (implemented):**

| Type | Value | Resource | Example Rights |
|------|-------|----------|---------------|
| `CAP_CHANNEL` | 1 | IPC channel endpoint | send, receive, grant |
| `CAP_SHARD` | 2 | Shard management | (reserved) |
| `CAP_MEMORY` | 3 | Memory region | (reserved) |
| `CAP_GPU_DMA` | 4 | GPU DMA access | write |

**Planned types (not yet implemented):** `CAP_VRAM`, `CAP_GPU`, `CAP_IRQ`, `CAP_IO`, `CAP_TIMER`.

Capabilities can be:
- **Granted:** A shard can pass a capability (or a restricted version) to another shard via `SYS_CAP_GRANT` (requires `RIGHT_CHANNEL_GRANT`).
- **Restricted:** Rights can be removed but never added (monotonic AND via `SYS_CAP_RESTRICT`).
- **Revoked:** A shard can revoke its own capabilities via `SYS_CAP_REVOKE` (non-cascading).
- **Inspected:** `SYS_CAP_INSPECT` returns packed `(cap_type << 48 | resource_id << 16 | rights)`.

### 5.2 pledge_gpu and unveil_vram

Inspired by OpenBSD's `pledge(2)` and `unveil(2)`:

**Implemented as syscalls:**

- `SYS_GPU_PLEDGE(41)` — `a0` is a bitmask of allowed syscall categories. Monotonic: can only remove bits, never add. Bits: `PLEDGE_SERIAL(1)`, `PLEDGE_CHANNEL(2)`, `PLEDGE_GPU_DMA(4)`.
- `SYS_GPU_UNVEIL(42)` — `a0=offset`, `a1=size`. One-shot: locks a VRAM range for DMA. Only the unveiled range can be used as a DMA source. Cannot be called again after the first call.

**Example: locked-down GPU HAL shard**

After initialization, the HAL shard restricts itself:
1. `pledge_gpu(PLEDGE_SERIAL | PLEDGE_CHANNEL | PLEDGE_GPU_DMA)` — only serial, IPC, and DMA allowed
2. `unveil_vram(offset, size)` — only a specific VRAM region can be used for DMA

From this point, any attempt to invoke other syscalls or DMA outside the unveiled region is rejected.

**Future expansion:** The design supports richer pledge categories (compute, alloc, shader_load) and multi-region unveil, to be added as the inference stack matures.

### 5.3 W^X for GPU Memory

All GPU memory regions enforce W^X (write XOR execute):
- **Shader code:** Loaded into a region marked executable, then made read-only. Cannot be written to after load.
- **Data buffers:** Marked read-write but never executable. Cannot be used as shader code.
- **Command buffers:** Marked write (CPU-side) and read (GPU-side). Cannot be used for shader execution.

This prevents GPU-based code injection attacks.

### 5.4 GPU ASLR

GPU memory layout is randomized per shard:
- VRAM region base addresses are randomized within the partition
- Command queue ring buffer locations are randomized
- Shader code load addresses are randomized
- Entropy: minimum 20 bits for VRAM regions, 16 bits for command queues

This makes VRAM-based exploits (buffer overflows, use-after-free) significantly harder to weaponize.

### 5.5 IOMMU and DMA Security

The IOMMU (AMD-Vi / Intel VT-d / ARM SMMU) is the hardware root of GPU isolation:

- Each GPU HAL shard has its own IOMMU domain
- DMA regions are configured by the supervisor before the HAL shard starts
- The HAL shard cannot modify its own IOMMU mappings
- DMA is restricted to the shard's assigned physical memory regions
- Interrupt remapping is enabled to prevent MSI spoofing

**DMA region lifecycle:**

```
1. Supervisor creates physical memory region
2. Supervisor configures IOMMU mapping for HAL shard's device
3. HAL shard can now DMA to/from the mapped region
4. On shard destroy: supervisor removes IOMMU mapping, zeroes memory
```

### 5.6 Side-Channel Mitigations

| Attack Vector | Mitigation | Status |
|--------------|------------|--------|
| FPU/SSE register leakage | `fninit` + zero all XMM0-15 + reset MXCSR on every context switch | Implemented |
| Debug register persistence | Clear DR0-DR3, reset DR7 on every context switch | Implemented |
| Branch predictor cross-shard inference | IBPB (wrmsr 0x49) on every context switch when CPU supports it | Implemented |
| User-mode timing attacks | CR4.TSD set — `rdtsc`/`rdtscp` causes #GP in ring 3 | Implemented |
| FXSAVE/FXRSTOR state leakage | Timer ISR saves/restores per-shard SSE state; side-channel clear still runs between shards | Implemented |
| GPU cache timing | Partition-level cache flushing on context switch | Planned |
| VRAM access patterns | Constant-time memory access primitives for sensitive operations | Planned |
| Speculative execution (CPU) | IBPB implemented; additional Spectre mitigations planned | Partial |
| PCIe bus snooping | Out of scope (physical access); mitigated by IOMMU for software DMA attacks | N/A |

### 5.7 Audit System (Planned)

All security-relevant events will be logged to a tamper-evident audit log:

- Shard creation/destruction
- Capability grants, delegations, and revocations
- `pledge_gpu` and `unveil_vram` calls
- IOMMU configuration changes
- Security policy violations (attempted access beyond capabilities)
- GPU fault events (page faults, command errors)

The audit log is written to a dedicated audit shard that has no GPU access and no network access (write-only, append-only). Log integrity is protected by a hash chain.

---

## 6. Boot Process

### 6.1 Boot Sequence (Implemented)

```
Power On
   │
   ▼
┌─────────┐
│  UEFI   │  Platform firmware
└────┬────┘
     │
     ▼
┌──────────────┐
│  Bootloader  │  coconut-boot (Rust, UEFI application)
│              │  - Load supervisor ELF from boot FS
│              │  - Parse PT_LOAD segments → 0x200000
│              │  - Build BootInfo + memory map
│              │  - Find ACPI RSDP via UEFI config table
│              │  - Exit boot services
│              │  - Jump to supervisor (RDI = BootInfo*)
└──────┬───────┘
       │
       ▼
┌──────────────────────────────┐
│  Boot Trampoline (_start)    │
│  - Set temp stack at 0x300000│
│  - Zero BSS, init serial     │
│  - Build 3-region page tables│
│  - Enable NXE, switch CR3    │
│  - Jump to supervisor_main   │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│  supervisor_main             │
│  - PMM, frame alloc, GDT,   │
│    TSS, IDT, PIC, PIT       │
│  - CR4.OSFXSR + CR4.TSD     │
│  - Detect IBPB, init ACPI   │
│  - PCI enum, IOMMU, GPU     │
│  - Init filesystem (ext2)   │
│  - Remove identity mapping   │
│  - Create shards:            │
│    GPU HAL ×2, fs-reader,   │
│    hello-c, llama-inference  │
│  - Enable interrupts         │
│  - Enter scheduler run loop  │
└──────────────────────────────┘
```

### 6.2 Boot Configuration (Planned)

Currently, shards are statically embedded in the supervisor binary via `include_bytes!` and created in `supervisor_main`. A future boot configuration system will support dynamic shard manifests:

```toml
# /boot/coconut.toml (planned)

[supervisor]
binary = "/boot/supervisor.elf"
log_level = "info"
audit = true

[gpu]
# Partition strategy: "equal" | "manifest" | "manual"
partition_strategy = "manifest"
zero_vram_on_boot = true

[[gpu.device]]
pci_slot = "0000:03:00.0"
vendor = "amd"
driver = "/boot/drivers/amd-rdna3.shard"

[[gpu.device]]
pci_slot = "0000:04:00.0"
vendor = "amd"
driver = "/boot/drivers/amd-cdna3.shard"

[network]
shard = "/boot/shards/network.shard"
interfaces = ["enp5s0"]

[filesystem]
shard = "/boot/shards/filesystem.shard"
root_device = "/dev/nvme0n1p2"

[audit]
shard = "/boot/shards/audit.shard"
log_path = "/var/log/coconut-audit"

# Inference shards to boot automatically
[[shard]]
name = "llama-inference"
manifest = "/etc/shards/llama.toml"
autostart = true

[[shard]]
name = "whisper-inference"
manifest = "/etc/shards/whisper.toml"
autostart = false
```

### 6.3 Shard Hot-Restart (Planned)

Shards can be restarted without rebooting the supervisor:

1. **Fault detection:** Supervisor detects shard crash (page fault, illegal instruction, GPU fault, watchdog timeout).
2. **Teardown:** All shard threads halted, GPU queues drained, VRAM zeroed, capabilities revoked.
3. **Rebuild:** Shard re-created from its original manifest, binary reloaded, GPU partition reassigned.
4. **State recovery:** For stateless inference shards, this is a clean restart. For stateful shards (filesystem), a recovery protocol is invoked.

Hot-restart latency target: <100ms for inference shards (dominated by GPU partition setup and model weight DMA reload is avoided via copy-on-write VRAM snapshots — see [Section 8](#8-memory-management)).

---

## 7. Scheduler Design

### 7.1 Three-Level Scheduling

```
Level 1: Supervisor Scheduler (CPU)
   │
   │  Assigns CPU time slices to shards
   │  Round-robin with priority classes
   │
   ├──── Level 2: Intra-Shard CPU Scheduler
   │        │
   │        │  Cooperative scheduling within a shard
   │        │  Shard manages its own threads
   │        │  Preemption only at shard boundary
   │
   └──── Level 3: GPU Scheduler
            │
            │  Per-partition GPU command queue management
            │  Deadline-aware for inference latency SLOs
            │  Co-scheduled with CPU to minimize idle bubbles
```

### 7.2 Level 1: Supervisor CPU Scheduler

The supervisor uses a simple, auditable scheduling algorithm:

- **Fixed-priority round-robin** with 4 priority classes:
  1. **Critical:** Supervisor-internal tasks, IOMMU management
  2. **High:** GPU HAL shards, interrupt-driven I/O
  3. **Normal:** Inference shards, application shards
  4. **Low:** Background maintenance

- **Time slice:** ~1ms (PIT at ~1 kHz, divisor 1193)
- **Preemption:** PIT timer ISR (vector 32) fires ~1 kHz. User-mode path: save GP regs + FXSAVE → `timer_preempt` (tick++, EOI, mark Ready, yield) → FXRSTOR + restore → iretq. Kernel-mode interrupts: EOI + iretq only (no preemption of syscall handlers).
- **Context switch:** Naked asm function — push/pop callee-saved registers (RBX, RBP, R12-R15), swap RSP.
- **Side-channel clearing:** `clear_sensitive_cpu_state()` zeroes FPU/SSE/debug state and issues IBPB before every shard switch.
- **MAX_SHARDS:** 8, each with a 4 KiB kernel stack.

### 7.3 Level 2: Intra-Shard CPU Scheduling

Within a shard, threads are cooperatively scheduled by a shard-local scheduler. The shard runtime library provides:

- `yield_now()` — Voluntarily yield the current thread's time slice.
- `spawn(task)` — Create a new cooperative task within the shard.
- `sleep(duration)` — Suspend the current task.

The supervisor does not see individual threads within a shard — it schedules the shard as an opaque unit.

### 7.4 Level 3: GPU Scheduler

Each GPU partition has a scheduler that manages command queue submission:

- **Deadline scheduling:** Inference shards declare latency SLOs (e.g., "complete within 50ms"). The GPU scheduler prioritizes dispatches to meet deadlines.
- **Batch coalescing:** Multiple small dispatches are coalesced into larger batches to amortize launch overhead.
- **Preemption:** GPU command preemption is hardware-dependent. On AMD RDNA3/CDNA3, mid-wave preemption is supported. The scheduler uses preemption to enforce deadlines.

### 7.5 CPU-GPU Co-Scheduling

Inference workloads alternate between CPU (tokenization, sampling) and GPU (matrix multiply, attention) phases. The scheduler co-schedules CPU and GPU work to minimize idle bubbles:

```
CPU:  [tokenize]────────────────[sample]────────────────[tokenize]
GPU:            [prefill████████]       [decode████████]
                ↑ CPU yields while     ↑ CPU yields while
                  GPU is active          GPU is active
```

Co-scheduling is achieved by:
1. The inference runtime signals phase transitions via lightweight supervisor calls.
2. The supervisor CPU scheduler deprioritizes shards that are GPU-bound (avoiding CPU waste).
3. The GPU scheduler fast-tracks shards whose CPU work just completed (minimizing GPU idle time).

### 7.6 Inter-Shard Coordination for Pipeline Parallelism

Large model inference can be split across multiple shards (pipeline parallelism):

```
Shard A (layers 0-15)  →  Shard B (layers 16-31)  →  Shard C (layers 32-47)
     GPU 0                      GPU 1                      GPU 2
```

The scheduler provides pipeline coordination primitives:

- **Pipeline barriers:** A shard can signal "my stage is complete" to the next shard in the pipeline.
- **Pipeline scheduling:** The supervisor schedules pipeline shards in wave order to maximize throughput.
- **Flow control:** Backpressure from downstream shards throttles upstream dispatch to prevent buffer overflow.

### 7.7 Real-Time and Power-Aware Scheduling

- **Real-time:** Inference shards can request soft real-time guarantees via manifest configuration. The scheduler reserves CPU and GPU capacity to meet latency SLOs.
- **Power-aware:** The scheduler monitors GPU temperature and power draw. When thermal limits approach, it reduces scheduling frequency or migrates work to cooler partitions. GPU power states (active/idle/sleep) are managed per-partition.

---

## 8. Memory Management

### 8.1 Supervisor-Level Physical Memory

The supervisor manages physical memory in large regions:

```
Physical Memory Map:
┌───────────────────────┐ 0x0000_0000_0000
│ Supervisor (reserved) │ Fixed mapping, <16 MiB
├───────────────────────┤
│ Shard Region Pool     │ Allocated to shards on demand
│                       │ 2 MiB granularity (huge pages)
├───────────────────────┤
│ IOMMU Page Tables     │ Managed by supervisor
├───────────────────────┤
│ Device MMIO           │ Mapped to HAL shards
└───────────────────────┘
```

The supervisor allocates physical memory in **2 MiB regions** to minimize page table overhead and TLB pressure. Regions are typed:

| Region Type | Properties |
|-------------|------------|
| `SupervisorPrivate` | Not accessible by any shard. Contains capability tables, scheduler state. |
| `ShardCode` | Mapped read + execute into one shard's address space. |
| `ShardData` | Mapped read + write into one shard's address space. |
| `ShardShared` | Mapped into multiple shards' address spaces (explicit grant required). |
| `DeviceDma` | Mapped into IOMMU for device DMA. Accessible by one HAL shard. |

### 8.2 Per-Shard Virtual Address Spaces

Each shard has its own virtual address space, configured by the supervisor:

```
Shard Virtual Address Space (implemented):
┌───────────────────────┐ 0x3F00_0000
│                       │ (unmapped)
├───────────────────────┤
│ GPU BARs (ASLR'd)    │ VRAM + MMIO (HAL shards only)
├───────────────────────┤ 0x0080_0000+
│                       │ (unmapped)
├───────────────────────┤
│ Stack (4 KiB)        │ R+W+NX
├───────────────────────┤ 0x007F_F000
│                       │ (unmapped)
├───────────────────────┤
│ Data (mmap'd heap)   │ R+W+NX (via SYS_MMAP)
├───────────────────────┤ 0x0010_0000+
│                       │ (unmapped)
├───────────────────────┤
│ Config page           │ R (HAL shards only, VA 0x4000)
├───────────────────────┤
│ Code (multi-page)    │ R+X (W^X enforced)
├───────────────────────┤ 0x0000_1000
│ (unmapped null guard) │
└───────────────────────┘ 0x0000_0000
```

GPU ASLR randomizes VRAM and MMIO BAR virtual addresses within [0x800000, 0x3F000000) per shard.

### 8.3 GPU Memory Management

GPU memory (VRAM) is managed separately from CPU memory:

```
GPU VRAM (per partition):
┌────────────────────────┐
│ Weights Region         │ Read-only after load
│ (contiguous, aligned)  │
├────────────────────────┤
│ KV-Cache Region        │ Grows with sequence length
│ (paged, 64 KiB pages)  │
├────────────────────────┤
│ Activations Region     │ Ephemeral per inference
│ (bump allocator)       │
├────────────────────────┤
│ Scratch Region         │ Temporary buffers
│ (pool allocator)       │
├────────────────────────┤
│ Command Buffers        │ Ring buffers for GPU queues
└────────────────────────┘
```

**Allocation strategies by type:**

| Type | Allocator | Rationale |
|------|-----------|-----------|
| `Weights` | Single contiguous allocation | Loaded once, never resized, needs contiguous VRAM for efficient access |
| `KvCache` | Paged allocator (64 KiB pages) | Grows dynamically with sequence length, needs efficient append |
| `Activations` | Bump allocator (reset per inference) | Allocated in order, freed all at once — bump is optimal |
| `Scratch` | Pool allocator (fixed-size blocks) | Reusable temporary buffers, predictable sizes |
| `CommandBuffer` | Ring buffer | Circular producer-consumer pattern |

### 8.4 DMA Management

DMA transfers between CPU and GPU memory are mediated by the supervisor:

1. **CPU → GPU (model load):** Filesystem shard reads model weights → shared memory region → GPU HAL shard DMA-copies to VRAM.
2. **GPU → GPU (pipeline parallel):** Shard A requests peer DMA to Shard B → supervisor verifies capabilities → configures IOMMU for cross-partition DMA → HAL performs transfer.
3. **GPU → CPU (inference output):** Inference shard DMA-copies output tokens to CPU-mapped region → IPC to requesting shard.

All DMA operations require explicit capability checks. The supervisor validates source and destination regions before any transfer.

### 8.5 Isolation Guarantees

| Property | Mechanism |
|----------|-----------|
| No cross-shard CPU memory access | Separate page tables per shard |
| No cross-shard GPU memory access | IOMMU domains + GPU hardware partitioning |
| No stale data in freed memory | Zero-on-free for both CPU and GPU memory |
| No stale data in allocated memory | Zero-on-alloc for `Activations` type |
| No executable data | W^X enforced on CPU and GPU memory |
| DMA containment | IOMMU restricts each device to assigned regions |

### 8.6 OOM Handling

coconutOS does **not** have swap. When a shard exceeds its memory quota:

1. **Soft limit:** Shard is notified via a callback. Expected to free caches (e.g., trim KV-cache).
2. **Hard limit:** Further allocations fail with `OutOfMemory`. The shard must handle this gracefully.
3. **Supervisor OOM:** If the supervisor itself is out of physical memory to assign, it refuses new shard creation. Existing shards are not killed — stability over throughput.

GPU OOM follows the same pattern: soft notification → hard alloc failure → no VRAM swap.

---

## 9. Inter-Process Communication (IPC)

### 9.1 Design Goals

- **Intra-shard IPC:** <500ns for synchronous message passing within a single shard
- **Inter-shard IPC:** <5µs for supervisor-mediated channel messages between shards
- **GPU DMA IPC:** Line-rate GPU-to-GPU transfer for pipeline parallelism
- **Zero-copy:** Large data transfers use shared memory, not message copying
- **Capability passing:** Capabilities can be transferred via IPC messages

### 9.2 Intra-Shard IPC

Within a shard, threads communicate without supervisor involvement:

| Mechanism | Latency | Use Case |
|-----------|---------|----------|
| Synchronous message passing | <500ns | Request-response between tasks |
| Shared memory (shard-local) | Memory access time | Bulk data sharing between threads |
| Events (wakeup signals) | <200ns | Signaling between producer/consumer tasks |

These are implemented entirely in the shard runtime library — no syscalls required.

### 9.3 Inter-Shard IPC

Communication between shards requires supervisor mediation:

#### 9.3.1 Channels (Implemented)

**Syscalls:** `SYS_CHANNEL_SEND(21)`, `SYS_CHANNEL_RECV(22)`.

**Implementation:**
- Single-buffered per direction, 256-byte max message size
- Blocking receive: shard state set to `Blocked`, scheduler yields
- Capability-gated: sender must hold `CAP_CHANNEL` with `RIGHT_CHANNEL_SEND`, receiver must hold `RIGHT_CHANNEL_RECV`
- Supervisor copies message between kernel-side buffers (no direct shard-to-shard memory access)
- Receiver is woken (state set to `Ready`) when a message arrives

#### 9.3.2 Shared Memory Fast Path

For bulk data transfer (e.g., inference inputs/outputs), channels are too slow. Shared memory regions provide zero-copy IPC:

```rust
/// Create a shared memory region accessible by two shards.
fn shared_memory_create(
    size: usize,
    owner_rights: Permission,
    peer_rights: Permission,
) -> Result<SharedMemoryHandle, IpcError>;

/// Grant access to a shared memory region to another shard.
fn shared_memory_grant(
    handle: &SharedMemoryHandle,
    target_shard: ShardId,
) -> Result<(), IpcError>;
```

Shared memory regions are:
- Created by the supervisor on behalf of the owning shard
- Mapped into both shards' virtual address spaces
- Protected by capability-based access control (read, write, or read-write per shard)
- Unmapped and zeroed on revocation or shard destruction

#### 9.3.3 GPU Peer-to-GPU DMA

For GPU-to-GPU data transfer between shards (pipeline parallelism):

```rust
/// Request a peer DMA transfer between GPU partitions of two shards.
fn gpu_peer_dma(
    src_alloc: &GpuAllocation,
    dst_shard: ShardId,
    dst_alloc: &GpuAllocation,
    size: usize,
) -> Result<FenceId, IpcError>;
```

The supervisor:
1. Verifies both shards hold `peer_copy` pledge
2. Verifies the source shard holds `dma_src` rights on the source allocation
3. Verifies the destination shard holds `dma_dst` rights on the destination allocation
4. Configures IOMMU for the transfer
5. Instructs the source GPU HAL shard to perform the DMA
6. Cleans up IOMMU mapping after transfer completes

### 9.4 Inference Pipeline Protocol

A standard IPC protocol for chaining inference stages:

```
┌─────────┐     ┌─────────┐     ┌─────────┐
│ Client  │────▶│ Shard A │────▶│ Shard B │────▶ Output
│         │ IPC │(layers  │ DMA │(layers  │
│         │     │ 0-15)   │     │ 16-31)  │
└─────────┘     └─────────┘     └─────────┘
```

**Protocol messages:**

| Message | Direction | Payload |
|---------|-----------|---------|
| `InferenceRequest` | Client → first shard | Input tokens, sampling params, session ID |
| `StageComplete` | Shard N → Shard N+1 | GPU DMA handle for intermediate activations |
| `InferenceResult` | Last shard → client | Output tokens, timing metadata |
| `PipelineSync` | Supervisor → all pipeline shards | Synchronization barrier for batch boundaries |

### 9.5 Capability Passing

Capabilities can be sent over IPC channels, enabling dynamic delegation:

```rust
let msg = IpcMessage::new()
    .with_data(request_bytes)
    .with_capability(vram_read_cap);  // Attach a VRAM read capability

channel_send(&endpoint, &msg)?;
```

The supervisor validates and transfers the capability during message dispatch. The sender can choose to:
- **Copy:** Both shards retain the capability.
- **Move:** The sender loses the capability; the receiver gains it.
- **Restrict:** The receiver gets a version with reduced rights.

---

## 10. Networking (Planned)

> **Not yet implemented.** This section describes the planned network architecture.

### 10.1 Architecture

The network stack runs entirely in a dedicated **network shard** — no networking code in the supervisor.

```
┌─────────────────────────────────────────────┐
│               Network Shard                  │
│  ┌──────┐  ┌──────┐  ┌──────┐  ┌────────┐ │
│  │ NIC  │  │  IP  │  │ TCP/ │  │  TLS   │ │
│  │Driver│──│Stack │──│ UDP  │──│        │ │
│  └──────┘  └──────┘  └──────┘  └────────┘ │
│       ▲                            │        │
│       │ MMIO + DMA                 │ IPC    │
│       │ (via IOMMU)                ▼        │
├───────┼──────────────────────────────────────┤
│       │        Supervisor (routing only)     │
├───────┼──────────────────────────────────────┤
│       │                                      │
│  NIC Hardware                                │
└──────────────────────────────────────────────┘
```

### 10.2 Per-Shard Network Isolation

Each shard has a declared network policy in its manifest:

| Policy | Meaning |
|--------|---------|
| `net.none` | No network access (default for inference shards) |
| `net.listen(port)` | Can accept incoming connections on a specific port |
| `net.connect(host, port)` | Can make outgoing connections to specific destinations |
| `net.unrestricted` | Full network access (network shard only) |

**Air-gapped inference:** By default, inference shards have `net.none` — they cannot initiate or receive network connections. Input/output flows through IPC channels to the application shard, which may have restricted network access. This prevents exfiltration of model weights or inference data.

### 10.3 RDMA and GPU-Direct Support

For high-performance multi-node inference:

- **RDMA:** The network shard can expose RDMA verbs to inference shards via IPC. RDMA buffer registration is mediated by the supervisor (IOMMU mapping).
- **GPU-Direct:** NIC-to-GPU DMA without CPU bounce buffers. Requires IOMMU configuration by the supervisor to allow the NIC to DMA directly to the GPU partition's VRAM.

```
Node A                              Node B
┌──────────┐    RDMA/RoCE    ┌──────────┐
│ GPU VRAM │◄───────────────▶│ GPU VRAM │
│ (Shard A)│    NIC-to-GPU   │ (Shard B)│
└──────────┘    DMA          └──────────┘
```

Both RDMA and GPU-Direct are opt-in, require explicit capabilities, and are mediated by the supervisor's IOMMU configuration.

---

## 11. Userland & Programming Model

### 11.1 Shard Deployment Manifest (Planned)

Currently, shards are statically compiled into the supervisor. A future manifest system will support dynamic deployment:

```toml
# /etc/shards/llama-70b.toml

[shard]
name = "llama-70b-inference"
binary = "/opt/shards/llama-inference.elf"
version = "1.0.0"

[resources]
cpu_cores = 4
memory_mib = 8192
gpu_device = "0000:03:00.0"
gpu_compute_units = 60       # Out of 120 total CUs
gpu_vram_mib = 40960         # 40 GiB VRAM

[security]
pledge_gpu = ["compute", "copy"]
unveil_vram = ["weights:ro", "activations:rw", "kv_cache:rw"]
network = "none"

[scheduling]
priority = "normal"
latency_slo_ms = 100        # Soft real-time target
cpu_affinity = [4, 5, 6, 7]

[model]
path = "/models/llama-70b-f16.coconut"
format = "coconut-model-v1"

[pipeline]
stage = 0                   # First stage in pipeline
next_shard = "llama-70b-stage1"
```

### 11.2 Inference Runtime API

coconutOS provides two runtime APIs for building shards:

**Rust API (`coconut-rt`):** Provides `#![no_std]` entry point, syscall wrappers, serial I/O macros, and GPU primitives (VramAllocator, CommandRing, matmul_4x4). Used by the GPU HAL shard (`coconut-shard-gpu`).

**C API (`coconut.h`):** Header-only syscall wrappers for freestanding C code. Used by hello-c and the llama-inference shard.

The following shows the planned high-level inference API (not yet implemented):

```rust
use coconut_runtime::{Shard, InferenceEngine, GpuContext};

fn main() -> Result<(), coconut_runtime::Error> {
    // Initialize the shard runtime
    let shard = Shard::init()?;

    // Get GPU context (partition already assigned by supervisor)
    let gpu = shard.gpu_context()?;

    // Load model weights into VRAM
    let model = InferenceEngine::load_model(
        &gpu,
        "/models/llama-70b-f16.coconut",
    )?;

    // Apply security restrictions (cannot be undone)
    shard.pledge_gpu(&[GpuPledge::Compute, GpuPledge::Copy])?;
    shard.unveil_vram_for_model(&model)?;

    // Serve inference requests via IPC
    let endpoint = shard.ipc_endpoint("inference")?;
    loop {
        let request = endpoint.recv::<InferenceRequest>()?;
        let output = model.infer(&gpu, &request)?;
        endpoint.send(&request.reply_to, &output)?;
    }
}
```

### 11.3 C ABI and FFI (Implemented)

coconutOS provides `include/coconut.h` — a header-only C interface with inline asm syscall wrappers:

```c
// coconut.h — header-only, no libc dependency

// Core
void coconut_exit(uint64_t code);
uint64_t coconut_serial_write(const char *buf, uint64_t len);
uint64_t coconut_yield(void);
uint64_t coconut_mmap(uint64_t va_start, uint64_t num_pages);

// Filesystem
uint64_t coconut_fs_open(const char *path, uint64_t path_len);
uint64_t coconut_fs_read(uint64_t fd, void *buf, uint64_t max_len);
uint64_t coconut_fs_stat(uint64_t fd);
uint64_t coconut_fs_close(uint64_t fd);

// IPC
uint64_t coconut_channel_send(uint64_t ch, const void *buf, uint64_t len);
uint64_t coconut_channel_recv(uint64_t ch, void *buf, uint64_t max_len);

// Capabilities, GPU pledge/unveil, GPU DMA — also available
```

C shards are compiled with clang (freestanding x86-64), linked as flat binaries via `targets/shard.ld`, and embedded into the supervisor.

### 11.4 Debugging and Profiling Tools

| Tool | Purpose | Status |
|------|---------|--------|
| `coconut-trace` | Per-shard kernel instrumentation: syscall count/cycles, context switches, wall time | Done (milestone 3.5) |
| `coconut-prof` | Host-side Python script that parses serial profiling output into a formatted report | Done (milestone 3.5) |
| `coconut-audit` | Query the audit log. Filter by shard, capability type, time range | Planned |
| `coconut-top` | Real-time dashboard showing shard CPU/GPU utilization, memory usage, IPC throughput | Planned |
| `coconut-shard` | CLI tool for shard management: create, start, stop, restart, inspect, logs | Planned |

**coconut-trace** adds lightweight counters to each `ShardDescriptor`: total syscalls dispatched, RDTSC cycles spent in syscall dispatch, context switch count, and wall-clock time (PIT ticks from first schedule to exit). The supervisor prints a summary table to serial before halt. Overhead is minimal (~20 cycles per syscall for two RDTSC reads).

**coconut-prof** (`scripts/coconut-prof.py`) reads serial output and produces a formatted report with per-shard stats, totals, and syscall distribution percentages. It also extracts shard lifecycle events (create, exit, blocked). No external dependencies — stdlib only.

```bash
# Pipe QEMU output directly
./scripts/qemu-run.sh 2>&1 | python3 scripts/coconut-prof.py

# Or parse a saved log
./scripts/qemu-run.sh 2>&1 | tee /tmp/boot.log
python3 scripts/coconut-prof.py /tmp/boot.log
```

GDB remote debugging and raw serial output remain available. See [debugging.md](debugging.md).

---

## 12. Filesystem & Storage

### 12.1 Current Implementation: ext2 Ramdisk

coconutOS currently uses a minimal read-only ext2 filesystem backed by a 128 KiB ramdisk generated at compile time.

**Implementation:**
- ext2 revision 0, 1024-byte blocks
- Supports direct block pointers and single indirect blocks (files up to 268 KiB)
- Generated by `build.rs` — no external tools required
- Contains `hello.txt` (22 bytes) and `model.bin` (~87 KiB, deterministic transformer weights)
- Global open file table (`MAX_OPEN_FILES = 16`), per-shard fd ownership

**Syscalls:** `SYS_FS_OPEN`, `SYS_FS_READ`, `SYS_FS_STAT`, `SYS_FS_CLOSE`.

### 12.2 Future: Crash-Consistent Filesystem (coconutFS)

A more capable filesystem is planned for production use:

**Design goals:**
- Crash-consistent (no fsck required after unclean shutdown)
- Read-optimized (model weights are read-heavy, write-rare)
- Large-file friendly (model files are 10-200+ GiB)
- Zero-copy model loading support

### 12.2 Zero-Copy Model Loading

The critical performance path is loading model weights from NVMe into GPU VRAM:

```
┌──────────┐    mmap     ┌──────────┐   DMA    ┌──────────┐
│  NVMe    │───────────▶│ CPU RAM  │────────▶│ GPU VRAM │
│ (model   │  page-fault │ (pinned  │ GPU HAL  │ (weights │
│  file)   │  on demand  │  pages)  │  shard   │  region) │
└──────────┘             └──────────┘          └──────────┘
```

1. **mmap:** The filesystem shard maps the model file into a shared memory region (demand-paged).
2. **Pin:** Pages are pinned as they are faulted in, preventing eviction.
3. **DMA:** The GPU HAL shard DMAs directly from the pinned CPU pages to VRAM.
4. **Unpin:** CPU pages are unpinned and freed after DMA completes. The model now lives entirely in VRAM.

For shard hot-restart, VRAM weight regions can be preserved across restarts (the supervisor does not zero the weights partition if the same shard is restarting with the same model).

---

## 13. Development Roadmap

### Phase 0: CPU-Only Shard Model — Complete

**Goal:** Functional microkernel with CPU-only shards, no GPU support.

| Milestone | Deliverable | Status |
|-----------|------------|--------|
| 0.1 | Supervisor boots on x86-64 (QEMU), initializes memory, prints to serial | Done |
| 0.2 | Shard creation and destruction (single-threaded, no GPU) | Done |
| 0.3 | IPC channels between shards (synchronous message passing) | Done |
| 0.4 | Basic CPU scheduler (round-robin, preemption) | Done |
| 0.5 | Capability system (create, check, delegate, revoke) | Done |
| 0.6 | Minimal filesystem shard (read-only, ext2-compatible for bootstrapping) | Done |

### Phase 1: GPU Bring-Up — Complete

**Goal:** GPU HAL shard for AMD RDNA3/CDNA3, basic compute dispatch.

| Milestone | Deliverable | Status |
|-----------|------------|--------|
| 1.1 | GPU PCIe enumeration and IOMMU domain setup | Done |
| 1.2 | GPU HAL shard: device init, memory alloc, command queue | Done |
| 1.3 | Basic compute dispatch (4×4 matrix multiply via command ring) | Done |
| 1.4 | GPU memory management with typed allocations | Done |
| 1.5 | VRAM zeroing on free, W^X enforcement | Done |
| 1.6 | Performance baseline: compute throughput measurement | Done |

### Phase 2: Multi-Shard Isolation — Complete

**Goal:** Multiple inference shards with strong isolation on a single GPU.

| Milestone | Deliverable | Status |
|-----------|------------|--------|
| 2.1 | GPU partitioning (CU slicing, VRAM carving) | Done |
| 2.2 | Multiple GPU HAL shard instances (one per partition) | Done |
| 2.3 | Inter-shard GPU DMA (pipeline parallelism) | Done |
| 2.4 | `pledge_gpu` / `unveil_vram` enforcement | Done |
| 2.5 | GPU ASLR | Done |
| 2.6 | Side-channel isolation testing and hardening | Done |

### Phase 3: Inference Stack — In Progress

**Goal:** End-to-end LLM inference on coconutOS.

| Milestone | Deliverable | Status |
|-----------|------------|--------|
| 3.1 | Inference runtime library (Rust API) | Done |
| 3.2 | C ABI / FFI layer | Done |
| 3.3 | Port llama2.c as proof-of-concept inference shard | Done |
| 3.4 | Inference pipeline protocol (multi-shard pipeline parallelism) | Done |
| 3.5 | coconut-trace, coconut-prof tooling | Done |
| 3.6 | Benchmark: Llama 70B inference latency vs. Linux/ROCm baseline | Planned |

### Phase 4: Hardening & Multi-Vendor — Planned

**Goal:** Production hardening, additional GPU vendor support.

| Milestone | Deliverable | Status |
|-----------|------------|--------|
| 4.1 | Security audit of supervisor (external) | Planned |
| 4.2 | Fuzzing campaign (syzkaller-style for supervisor syscalls) | Planned |
| 4.3 | NVIDIA GPU HAL shard (Hopper/Blackwell) | Planned |
| 4.4 | Apple GPU HAL shard (M-series, ARM64 port) | Planned |
| 4.5 | Network shard with RDMA/GPU-Direct support | Planned |
| 4.6 | Formal verification of supervisor capability system (Verus or similar) | Planned |

---

## 14. Open Questions & Risks

### 14.1 GPU Driver Complexity

**Risk:** Even compute-only GPU drivers are ~50K LoC with complex hardware interactions. User-mode drivers may have performance overhead due to IOMMU and context switching.

**Mitigation:** Start with the smallest possible driver surface. Benchmark early. Accept some performance loss for isolation. The IOMMU overhead on modern hardware (AMD-Vi) is typically <5% for large DMA transfers.

### 14.2 GPU Side Channels

**Risk:** GPU side-channel attacks (cache timing, power analysis, memory access patterns) are an active research area. Hardware mitigations may be insufficient.

**Mitigation:** Design the architecture to support strong isolation, but acknowledge that side-channel resistance depends on hardware support. Partition-level cache flushing is a software mitigation, but hardware-enforced cache partitioning (AMD MIG-equivalent) is preferred. Track academic research and GPU vendor security roadmaps.

### 14.3 IOMMU Limitations

**Risk:** IOMMU granularity (typically 4 KiB pages) may be too coarse for fine-grained GPU memory isolation. Some GPU operations may bypass IOMMU (e.g., GPU-internal MMU, peer-to-peer over NVLink without IOMMU).

**Mitigation:** Use GPU hardware partitioning (CU slicing) as the primary isolation mechanism, with IOMMU as the backstop for DMA. Require GPU vendors to support IOMMU for all DMA paths. Refuse to support GPU interconnects that bypass IOMMU.

### 14.4 IPC Overhead

**Risk:** Supervisor-mediated IPC adds latency compared to Linux's direct function calls. For inference workloads that frequently alternate CPU and GPU phases, this could impact throughput.

**Mitigation:** Fast-path optimizations (register-based IPC for small messages, shared memory for bulk data). Target <5µs per inter-shard IPC, which is acceptable if shards batch their GPU work (typical GPU kernel is >100µs).

### 14.5 GPU Preemption

**Risk:** GPU command preemption is hardware-dependent and may not be reliable. A long-running GPU kernel in one shard could starve other shards.

**Mitigation:** Use cooperative preemption (shards yield at defined points in their compute kernels) as the primary mechanism. Rely on hardware preemption as a fallback. Set maximum GPU kernel execution time limits per shard.

### 14.6 Formal Verification Scope

**Risk:** Formally verifying even the small supervisor is a multi-year effort. Full functional correctness (seL4-level) may not be achievable in the initial timeline.

**Mitigation:** Start with property-level verification of critical invariants (capability safety, memory isolation) using tools like Verus (Rust) or Kani. Defer full functional correctness to a later phase. The small supervisor size (10K LoC) makes this more tractable than verifying a monolithic kernel.

### 14.7 Ecosystem and Adoption

**Risk:** A new OS with no ecosystem will struggle to attract users and contributors. Inference workloads depend on complex software stacks (PyTorch, CUDA, etc.) that won't be available on coconutOS.

**Mitigation:** Focus on the C ABI/FFI layer to enable porting of existing inference engines (llama.cpp, whisper.cpp). Don't try to replace CUDA — provide a compute-only API that inference engines can target. Position coconutOS as a deployment target (not a development environment) for security-critical inference.

---

## 15. Appendices

### Appendix A: Supervisor Syscall Table (Implemented)

| # | Name | Arguments | Description |
|---|------|-----------|-------------|
| 0 | `SYS_EXIT` | `a0`: exit code | Terminate shard |
| 1 | `SYS_SERIAL_WRITE` | `a0`: buffer ptr, `a1`: length | Write to serial console |
| 11 | `SYS_CAP_GRANT` | `a0`: handle, `a1`: target shard, `a2`: new rights | Grant capability copy |
| 12 | `SYS_CAP_REVOKE` | `a0`: handle | Revoke a capability |
| 13 | `SYS_CAP_RESTRICT` | `a0`: handle, `a1`: new rights | Restrict rights (monotonic AND) |
| 14 | `SYS_CAP_INSPECT` | `a0`: handle | Inspect capability |
| 21 | `SYS_CHANNEL_SEND` | `a0`: channel ID, `a1`: buffer ptr, `a2`: length | Send IPC message |
| 22 | `SYS_CHANNEL_RECV` | `a0`: channel ID, `a1`: buffer ptr, `a2`: max length | Receive IPC message (blocking) |
| 30 | `SYS_FS_OPEN` | `a0`: path ptr, `a1`: path length | Open file by path |
| 31 | `SYS_FS_READ` | `a0`: fd, `a1`: buffer ptr, `a2`: max length | Read from open file |
| 32 | `SYS_FS_STAT` | `a0`: fd | Get file size |
| 33 | `SYS_FS_CLOSE` | `a0`: fd | Close open file |
| 40 | `SYS_GPU_DMA` | `a0`: target partition, `a1`: src offset, `a2`: packed(dst<<32\|len) | Inter-partition VRAM copy |
| 41 | `SYS_GPU_PLEDGE` | `a0`: bitmask of allowed categories | Monotonic syscall restriction |
| 42 | `SYS_GPU_UNVEIL` | `a0`: offset, `a1`: size | Lock VRAM range for DMA |
| 43 | `SYS_MMAP` | `a0`: va_start (page-aligned), `a1`: num_pages | Map data pages into shard |
| 62 | `SYS_YIELD` | — | Yield CPU time slice |

Entry: `syscall` instruction → `syscall_entry` (naked stub) → dispatch by RAX.
SFMASK clears IF on entry — no timer interrupts during syscall handling.

### Appendix B: Hardware Requirements

**Minimum (development/testing):**

| Component | Requirement |
|-----------|-------------|
| CPU | x86-64 with IOMMU support (AMD-Vi or Intel VT-d) |
| RAM | 16 GiB |
| GPU | AMD RDNA3 (e.g., RX 7900 XTX) or CDNA3 (MI300) |
| Storage | NVMe SSD, 500 GiB |
| Firmware | UEFI with Secure Boot support |

**Recommended (production inference):**

| Component | Requirement |
|-----------|-------------|
| CPU | AMD EPYC (Zen 4+) with AMD-Vi |
| RAM | 128+ GiB DDR5 |
| GPU | 2-8x AMD MI300X (192 GiB HBM3 each) |
| Storage | NVMe SSD, 2+ TiB |
| Network | 100 GbE with RDMA (RoCEv2) |
| Firmware | UEFI with Secure Boot, TPM 2.0 |

### Appendix C: Comparison Matrix

| Feature | Linux | FreeBSD | OpenBSD | Fuchsia | seL4 | **coconutOS** |
|---------|-------|---------|---------|---------|------|-------------|
| Microkernel | No | No | No | Yes | Yes | **Yes** |
| GPU-native isolation | No | No | No | No | No | **Yes** |
| Capability-based security | Partial | Capsicum | No | Yes | Yes | **Yes** |
| pledge/unveil | No | No | Yes | No | No | **Yes (GPU-extended)** |
| W^X (CPU) | Partial | Partial | Yes | No | N/A | **Yes** |
| W^X (GPU) | No | No | N/A | N/A | N/A | **Yes** |
| GPU ASLR | No | No | N/A | N/A | N/A | **Yes** |
| GPU memory zeroing | No | No | N/A | N/A | N/A | **Yes** |
| Rust kernel | No | No | No | Partial | No | **Yes** |
| Formal verification | No | No | No | No | Yes | **Planned (milestone 4.6)** |
| TCB size | ~28M LoC | ~10M LoC | ~1M LoC | ~200K LoC | ~10K LoC | **<10K LoC** |

### Appendix D: Glossary

| Term | Definition |
|------|-----------|
| **Shard** | The fundamental unit of isolation in coconutOS. Combines a CPU address space, GPU partition, capability set, and threads. |
| **Supervisor** | The microkernel. The only code running in ring 0. Manages shards, capabilities, IPC, and scheduling. |
| **HAL** | Hardware Abstraction Layer. Defines Rust traits for GPU operations. Vendor-specific implementations run in user-mode shards. |
| **Capability** | An unforgeable token granting specific rights to a specific resource. |
| **Pledge** | A monotonic restriction on permitted operations (inspired by OpenBSD `pledge(2)`). |
| **Unveil** | A monotonic restriction on visible resources (inspired by OpenBSD `unveil(2)`). |
| **CU** | Compute Unit. The basic unit of GPU compute hardware (AMD terminology). Equivalent to SM (NVIDIA). |
| **VRAM** | Video RAM. GPU-local memory (HBM or GDDR). |
| **IOMMU** | Input-Output Memory Management Unit. Hardware that restricts DMA access by devices. |
| **DMA** | Direct Memory Access. Hardware-level memory transfer without CPU involvement. |
| **IPC** | Inter-Process Communication. Message passing or shared memory between shards. |
| **W^X** | Write XOR Execute. A memory policy where a page can be writable or executable, but never both. |
| **ASLR** | Address Space Layout Randomization. Randomizing memory layout to hinder exploitation. |
| **SLO** | Service Level Objective. A target for latency or throughput. |
| **TCB** | Trusted Computing Base. The set of components that must be correct for system security to hold. |
| **KV-Cache** | Key-Value Cache. Cached attention states in transformer inference, growing with sequence length. |
| **Pipeline parallelism** | Splitting a model across multiple GPUs/shards by layer groups. |

### Appendix E: References

1. Klein, G., et al. "seL4: Formal Verification of an OS Kernel." *SOSP 2009*.
2. de Raadt, T. "pledge() — a new mitigation mechanism." *OpenBSD*.
3. de Raadt, T. "unveil() — restrict filesystem view." *OpenBSD*.
4. Naghibijouybari, H., et al. "Rendered Insecure: GPU Side Channel Attacks are Practical." *IEEE S&P 2018*.
5. AMD. "RDNA 3 Instruction Set Architecture Reference Guide."
6. AMD. "AMD Instinct MI300 Series Accelerator ISA."
7. Heiser, G. "The seL4 Microkernel — An Introduction." *CSIRO/Data61*.
8. Zhu, Y., et al. "Understanding the Security of GPU Computing." *CCS 2017*.
9. Asahi Linux Project. "GPU Reverse Engineering Documentation."
10. The Rust Programming Language. "The Rustonomicon — Unsafe Rust."
