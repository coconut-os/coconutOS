# Code Style

## Formatting

Use default rustfmt settings (edition 2021). No rustfmt.toml overrides. If the formatter does it, don't fight it.

4-space indentation. No tabs. 100-column soft limit for code; comments can run longer if breaking them hurts readability.

## Naming

- **Constants**: `SCREAMING_CASE`. Group by prefix: `PTE_`, `SYS_`, `CAP_`, `RIGHT_`.
- **Functions and variables**: `snake_case`.
- **Types and enums**: `CamelCase`.
- **Statics**: `SCREAMING_CASE` (`static mut CURRENT_SHARD: usize`).

Names must be descriptive. A variable's purpose should be obvious from its name alone. `id` is fine in a 5-line function where context is clear. `x` is never fine.

## Comments

### When to comment

Comment *why*, never *what*. If the code needs a comment to explain what it does, rewrite the code. The exception is hardware interaction, memory layout, and assembly — there, comment *what* AND *why* because the "what" is genuinely non-obvious.

```rust
// Good: explains why
// SFMASK clears IF on syscall entry — prevents timer interrupts during dispatch
syscall::init();

// Bad: restates code
// Initialize the syscall module
syscall::init();
```

### Module-level docs

Every source file starts with a `//!` block (1-3 lines) stating what the module does and its key invariants. No boilerplate, no filler.

```rust
//! Physical memory manager — bitmap allocator for 2 MiB regions.
//!
//! Initialized from BootInfo memory map. Reserves supervisor code and
//! boot page table regions. Thread-safety: single-core only (no locks).
```

### Section separators

Use comment dividers to separate major sections within a file. This is a kernel — files are often long and dense. Visual structure matters.

```rust
// ---------------------------------------------------------------------------
// Page table construction
// ---------------------------------------------------------------------------
```

Use `// ===...===` sparingly, only for top-level divisions in assembly-heavy code (like the boot trampoline).

### Inline comments for hardware and addresses

Always annotate magic numbers, hardware registers, and memory addresses:

```rust
const SUPERVISOR_LOAD_ADDR: u64 = 0x200000;  // 2 MiB — above legacy BIOS area
```

### Function documentation

Use `///` doc comments for public API functions that other modules call. Keep them to 1-2 lines. If a function needs a paragraph of documentation, it's doing too much.

Private helper functions don't need doc comments — a descriptive name is enough. Add an inline comment if the *reason* for the function's existence isn't obvious.

## Imports

Group imports in this order, separated by blank lines:

1. `core::` / `alloc::` (standard library)
2. External crates (there should be almost none)
3. Crate-internal (`crate::`, `super::`)

Within each group, sort alphabetically. Prefer specific imports over globs.
