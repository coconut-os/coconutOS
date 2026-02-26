# Safety and Correctness

## Unsafe Code

Every `unsafe` block must have a comment directly above it explaining **why it is sound**. Not "this is unsafe because we use raw pointers" — that restates the obvious. Explain the invariant that makes it safe:

```rust
// Sound: SHARDS is initialized at static level, we're single-core,
// and this runs only after PMM init with interrupts disabled.
let shard = unsafe { &mut (*(&raw mut SHARDS))[id] };
```

If you can't articulate why an unsafe block is sound, the code is wrong.

### Raw pointer access

Always use `&raw mut` / `&raw const` for creating pointers to statics. Never use `&mut STATIC` or `&STATIC` — these create references with aliasing guarantees we can't uphold in kernel code.

### Naked functions

Use `#[unsafe(naked)]` with `naked_asm!()`. Document register conventions at the top of the assembly block:
- What registers hold arguments on entry
- What registers are preserved
- What the function returns and where

### Volatile access

Use `core::ptr::read_volatile` / `write_volatile` for hardware registers and page table entries that may be read by the CPU asynchronously. Comment why volatility is needed.

## Assertions

Use `assert!()` liberally for invariants that, if violated, indicate a kernel bug. Include a message:

```rust
assert!(boot_info.magic == BOOT_INFO_MAGIC, "BootInfo magic mismatch");
```

A kernel panic from a failed assertion is better than silent corruption. In debug builds, assertions are our primary bug-detection tool. In release builds, they're our last line of defense.

Do NOT use `debug_assert!()` in kernel code. If an invariant matters, check it always.

## Syscall Boundary

The syscall boundary is the security perimeter. Every syscall handler must:

1. **Validate all user-provided values** before use — pointers, lengths, indices, fd numbers
2. **Bounds-check buffer pointers** against the shard's user address space
3. **Check capabilities** before granting access to any resource
4. **Return `u64::MAX`** for errors — never panic on bad user input
5. **Never trust user pointers** — validate they fall within mapped user pages

A syscall handler that panics on malformed input is a DoS vulnerability.

## Memory Safety

- Zero memory before freeing frames (`frame::dealloc` should zero)
- Never leave stale mappings in page tables after shard destruction
- Clear capability tables on shard destroy
- Check for integer overflow on pointer arithmetic from user values

## No Silent Failures

If something fails in the kernel, it should be visible:
- `serial_println!()` for diagnostic messages (prefixed with subsystem name)
- `panic!()` for unrecoverable kernel bugs
- `u64::MAX` return for recoverable syscall errors

Never swallow an error. Never return a default value when something went wrong.
