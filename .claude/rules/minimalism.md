# Minimalism

## The Rule

If a feature is not required by the current milestone, it does not exist. Not stubbed. Not planned with a TODO. Not behind a flag. It does not exist.

When the milestone that needs it arrives, implement exactly what that milestone requires. Not more.

## What This Means in Practice

### No speculative code

```rust
// WRONG: "we might need this later"
pub struct ShardDescriptor {
    // ...
    pub gpu_context: Option<GpuContext>,  // Phase 1
    pub network_buffer: Option<NetBuf>,   // Phase 4
}

// RIGHT: add fields when the milestone that uses them lands
pub struct ShardDescriptor {
    // ...
}
```

### No unnecessary abstractions

Don't create a trait for something with one implementation. Don't create a helper function called from one place. Don't create a module for 20 lines of code.

Three similar lines of code is better than a premature abstraction. When duplication becomes a maintenance burden (not before), factor it out.

### No defensive overengineering

Don't add error handling for scenarios that can't happen in the current architecture. Don't add configuration for things that have exactly one correct value. Don't add fallback paths that will never execute.

```rust
// WRONG: the supervisor is single-core, there's no contention
fn alloc_frame() -> Option<u64> {
    let _lock = FRAME_LOCK.lock();  // "just in case"
    // ...
}

// RIGHT: document the assumption
// Single-core: no locking needed. Revisit when adding SMP.
fn alloc_frame() -> Option<u64> {
    // ...
}
```

### No dead code

If code isn't called, delete it. If a constant isn't used, delete it. If a function was scaffolding for development, remove it before committing. `#[allow(dead_code)]` at the module level is acceptable only during active development of a milestone — clean it up before the milestone is marked complete.

`git` remembers everything. Deleted code is one `git log` away.

### Dependencies

The supervisor has zero external runtime dependencies. Keep it that way. Every external crate is code we don't control, can't audit line-by-line, and must update forever.

The bootloader uses `uefi` because reimplementing UEFI protocol bindings would be pointless. That's the bar: a dependency is justified only when reimplementing it would be absurd.

### Comments and documentation

Document what exists, not what might exist. Don't write "TODO: add GPU support here" in code. The roadmap tracks future work. The code tracks current reality.

Inline comments should explain the code that's there. If a comment describes something that isn't implemented, delete the comment.

### File count

Fewer files is better. A new file is overhead: one more thing to navigate, one more module boundary to maintain. Add files only when a module genuinely owns a distinct concern.
