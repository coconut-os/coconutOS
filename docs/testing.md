# Testing

coconutOS is tested by booting it. The supervisor runs every shard to completion, and the serial log is the test output. There is no separate test harness: a boot either reaches `all shards completed` cleanly or it fails.

## What CI Checks

`.github/workflows/ci.yml` runs on every push and pull request to `main`, and on version tags. It builds everything from a clean checkout with the toolchain pinned in `rust-toolchain.toml`, boots the image in QEMU, and stops QEMU once the supervisor prints `Halting.` (or panics). The boot log is then checked:

| Check | Fails when |
|-------|-----------|
| Supervisor completes | `all shards completed` is missing |
| No kernel panic | `KERNEL PANIC` appears |
| No shard killed | any `, killing` line appears (a shard hit a page fault or general protection fault) |
| Syscall tests ran and passed | `FAILURES DETECTED` appears, or `ALL TESTS PASSED` is missing |

On a `v*` tag, CI also attaches the boot image to the GitHub release.

## Running the Same Checks Locally

`./scripts/qemu-run.sh` builds and boots, but QEMU keeps running after the supervisor halts. To get a log and an exit like CI:

```bash
./scripts/qemu-run.sh > boot.log 2>&1 &
until grep -qE "^Halting\.|KERNEL PANIC" boot.log || ! kill -0 $! 2>/dev/null; do sleep 1; done
pkill -f qemu-system-x86_64
grep -E "KERNEL PANIC|, killing|FAILURES DETECTED|ALL TESTS PASSED|all shards completed" boot.log
```

## The Syscall Test Shard

`shards/fuzz-syscall` runs at `Low` priority, after the other shards, in two phases:

1. **Deterministic checks** call every syscall with adversarial arguments: null, kernel, and overflowing pointers, zero and oversized lengths, out-of-range handles and descriptors, missing capabilities, and buffers in each ELF segment.
2. **A random campaign** (below) issues thousands of generated syscalls and then checks that nothing broke.

Each check prints `PASS: <name>` or `FAIL: <name>`, and the shard ends with a summary:

```
fuzz: campaign seed=1438430000 iterations=2000 accepted=455
PASS: campaign: text and rodata unchanged
PASS: campaign: no descriptors leaked
...
--- Fuzz Summary ---
fuzz: pass=78 fail=0 total=78
fuzz-syscall: ALL TESTS PASSED
```

The GPU tests run last because they call `SYS_GPU_PLEDGE`, which restricts every syscall that follows.

To reach code past the first capability check, the supervisor grants this shard `CAP_CHANNEL(2)` with `SEND` and `GRANT` at boot. It never gets `RECV`, so a generated receive can never block.

### Adding a Check

Add a call to `check()` in the matching `test_*` function:

```c
r = coconut_fs_read(fd, (void *)0x1000, 64);
check(r == COCONUT_ERROR, "fs_read: into .text rejected");
```

A good check states the boundary it tests in its name, and fails if the kernel accepts what it should reject (or the reverse).

### Making Sure a Check Can Fail

A check that never fails proves nothing. When adding one for a kernel rule, temporarily break the rule in the supervisor and confirm the check reports `FAIL:`. For example, removing the data-segment clause from `validate_user_write_buf` in `syscall.rs` makes `fs_read: into .bss accepted` fail.

## Random Syscall Campaign

The campaign is modelled on [syzkaller](https://github.com/google/syzkaller): instead of uniform noise, each argument comes from a generator for its type, biased towards values at the edges of what the kernel accepts.

| Argument type | Values drawn from |
|---------------|-------------------|
| Pointers | buffers in `.bss` and the heap, text and rodata, the null page, unmapped holes, region edges, non-canonical and kernel addresses, values that wrap |
| Lengths | `0`, `1`, `255`-`257`, `4095`-`4097`, 32-bit boundaries, `u64::MAX`, random |
| Handles, fds, IDs | in range, last valid, first invalid, high bits set above bit 31, random |
| Descriptors | descriptors returned by earlier `SYS_FS_OPEN` calls |
| Syscall numbers | every syscall except `SYS_EXIT` and `SYS_GPU_PLEDGE`, plus unassigned numbers and numbers that differ only above bit 31 |

Buffers the kernel may write into are confined to dedicated scratch areas, so an accepted call cannot corrupt the fuzzer itself, and `SYS_SERIAL_WRITE` only ever echoes a buffer of dots.

After the campaign the shard checks that:

- the shard is still running (a fault would have killed it, and CI fails on `, killing`),
- its text and rodata are byte-for-byte unchanged (no syscall wrote through a read-only mapping),
- after closing every descriptor it may hold, all 16 entries of the shared open-file table are free.

A kernel panic anywhere in the run fails CI as well.

### Seeds and Reproducing a Run

Each boot seeds the campaign from the TSC and prints the seed. Every CI run therefore explores a different sequence, and any failure can be replayed exactly:

```bash
FUZZ_SEED=1438430000 ./scripts/qemu-run.sh
```

`FUZZ_ITERS` sets the number of generated syscalls (default 2000). For a longer local hunt:

```bash
FUZZ_ITERS=20000 ./scripts/qemu-run.sh
```

Both are compile-time defines for the fuzz shard; changing them rebuilds that shard and relinks the supervisor.

## Reading a Failure

- **`FAIL: campaign: ...`** — the random campaign broke an invariant. Replay it with the printed `FUZZ_SEED`.
- **`FAIL: <name>`** — the kernel returned something other than what the check expected. The lines just before it usually include the kernel's diagnostic for that syscall.
- **`PF: shard N faulted at RIP ..., addr ..., killing`** — a shard touched memory it has no mapping for, or wrote to a read-only page. Find the instruction with `llvm-objdump -d target/<shard>/<shard>.elf`. See [Debugging](debugging.md).
- **`KERNEL PANIC`** — an assertion in the supervisor failed. The panic message names the file, line, and broken invariant.
