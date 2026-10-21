# Story, Vision and Ethos

Why coconutOS exists, where it is going, and the rules it keeps on the way. For how it works, see [architecture.md](architecture.md).

## The story

coconutOS started as a joke. In 2023 it was a Linux live image with a tongue-in-cheek motto, "the unapologetically one and only true OS", and a patron saint in [Richie Guix](https://www.youtube.com/watch?v=lE4UXdJSJM4), whose verdict on his own distro was *"Breaks more often, just how I like it."* That image is still around as [`coconut-mklive`](https://github.com/coconut-os/coconut-mklive), archived.

Joke about building your own OS for long enough and you end up building one. The question that turned the joke into a kernel was a plain one: what keeps one model from reading another model's memory when they share a GPU?

On a mainstream stack the honest answer is "not much". The GPU driver runs in ring 0, and it is hundreds of thousands of lines of code (`amdgpu`) or tens of millions (NVIDIA's). VRAM is not zeroed between tenants by default. Nothing like `pledge(2)` exists for GPU compute, and nothing like `unveil(2)` exists for VRAM. Isolation on GPUs was added after the fact. Every workload ends up trusting the most complex code in the machine.

coconutOS turns that around. The supervisor is a few thousand lines of Rust with no external dependencies at runtime. GPU drivers run as unprivileged shards. Every resource is reached through a capability that can be granted, restricted and revoked, and there is no root.

## Why a coconut

A coconut is three carpels fused into one hard shell. Each carpel has an eye, but only one eye is soft enough for the sprout to get through. The other two stay sealed. Nothing gets in or out except through the one opening that was meant for it.

That is the design, and it is what the mark shows. A shard is a piece of the shell: its own page tables, its own GPU partition, its own capability set. The open eye is the capability a shard was granted. The sealed eyes are every other path, and they stay sealed. There is no ambient authority to fall back on, no root, and no fourth eye.

## Vision

When CPUs became shared infrastructure, the industry stopped trusting the host OS to keep tenants apart. It used hardware virtualization instead, enforced by the CPU itself, and the cloud was built on that guarantee. GPUs are now the most valuable computer in the building, and they have no equivalent.

coconutOS is a bet on what that equivalent looks like:

- **One shell per model.** Every inference workload gets its own address space, VRAM partition and capability set. Nothing is shared unless it was granted.
- **Drivers outside the kernel.** A bug in a GPU driver takes down one shard, not the machine.
- **Monotonic restriction.** A shard can give up privileges with `pledge_gpu` and `unveil_vram`, but it can never get them back. A compromised shard cannot undo its own containment.
- **A trusted computing base you can read.** The supervisor stays small enough for one person to audit, and eventually small enough to verify formally.
- **Every vendor.** The isolation model belongs to the kernel, not to any one GPU. AMD comes first, then NVIDIA and Apple.

The goal is not to replace Linux. It is to show, with code that boots and runs a transformer, that GPU isolation can be a kernel primitive rather than an afterthought. If that idea ends up in Linux, in a hypervisor, or in the next GPU architecture instead, the bet still paid off.

## Ethos

coconutOS follows OpenBSD's approach to systems software: **correct, minimal, readable, secure**, in that order.

- **Every line justifies itself.** No feature exists before the milestone that needs it: not stubbed, not behind a flag, not left as a TODO. Code that isn't called is deleted, and git remembers it.
- **No dependencies we could reasonably write ourselves.** The supervisor depends on nothing outside this repository. The bootloader uses `uefi`, because rewriting UEFI bindings would be absurd. That is the bar.
- **Break loudly.** Richie's distro broke often. This kernel breaks loudly. Mapping over a page that is already present panics. Every random syscall run prints its seed so a failure can be replayed exactly. CI fails on a killed shard, not only on a panic. A silent failure is a security bug.
- **Say what is true.** The README states what works today and nothing more. Today coconutOS runs in QEMU, with an emulated IOMMU and a GPU whose compute is simulated on the CPU. The isolation logic is real. Real silicon comes next.
- **Explain why.** Comments, commit messages and docs explain why something is the way it is. The code already shows what it does.
- **Keep the joke.** The name stays, the coconut stays, and so does the humour. A serious kernel doesn't need a serious mascot.

## Non-goals

- A desktop or general-purpose OS.
- POSIX compatibility.
- Running existing Linux GPU drivers.
- Speed bought with isolation. Where the two conflict, isolation wins until a profiler and a threat model both say otherwise.
