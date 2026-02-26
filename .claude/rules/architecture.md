# Architecture Rules

## Crate Boundaries

The three crates have strict dependency rules:

- **coconut-shared**: Zero dependencies. `#![no_std]`. Only `#[repr(C)]` types and constants. Both other crates depend on it. Never add runtime logic here.
- **coconut-boot**: Depends on `coconut-shared` and `uefi`. No other crates. This is a UEFI application — it exits boot services and never runs again.
- **coconut-supervisor**: Depends on `coconut-shared` only. **Zero external runtime dependencies.** Every line of kernel code must be ours. If you need functionality from a crate, read how it works and implement the subset we need.

## Module Organization

One module per responsibility. A module should do one thing and its name should say what. If you can't name it in one or two words, it's doing too much.

Current supervisor modules and their boundaries:

| Module | Owns | Does NOT touch |
|--------|------|---------------|
| `pmm` | Physical memory bitmap | Page tables |
| `frame` | 4 KiB frame allocation | Anything above frame granularity |
| `vmm` | Page table operations, HHDM | Physical allocation decisions |
| `shard` | Shard descriptors, lifecycle | Scheduling policy |
| `scheduler` | Run queue, context switch | Shard creation |
| `syscall` | MSR setup, dispatch routing | Syscall implementation logic |
| `channel` | IPC buffers | Capability checks (delegates to capability) |
| `capability` | Cap tables, grant/revoke | Channel internals |
| `ext2` | Ext2 parsing on `&[u8]` | File descriptors, ownership |
| `fs` | Open file table, fd ops | Ext2 format details (delegates to ext2) |
| `gdt`, `idt`, `tss` | CPU descriptor tables | Each other |
| `pic`, `pit` | Hardware drivers | Scheduler policy |
| `serial` | UART I/O | Everything else |

Respect these boundaries. If a change requires module A to know about module B's internals, the abstraction is wrong — fix the interface.

## Adding New Modules

Before adding a new `.rs` file, ask: can this live in an existing module? A new file is justified only when it owns a distinct resource or abstraction that doesn't belong anywhere else.

New modules must have:
1. A `//!` doc block explaining what they own
2. A clear `pub` API surface — keep it minimal
3. An `init()` function if they have global state (called from `supervisor_main`)

## Global State

Global mutable state (`static mut`) is acceptable in kernel code where the alternative is worse. But every static mut must:
1. Be accessed only through `&raw mut` / `&raw const`
2. Have a comment stating its initialization point and access pattern
3. Be single-writer where possible (we're single-core for now)

When we go multi-core, each static mut becomes a lock candidate. Keep them few and well-documented.

## No Premature Abstraction

Do not create traits, generics, or abstractions until there are at least two concrete users. A struct with one implementation doesn't need a trait. A function called from one place doesn't need to be generic.

Write the concrete thing. When a second use appears, refactor. Not before.
