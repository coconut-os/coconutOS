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

`shards/fuzz-syscall` runs at `Low` priority, after the other shards, and calls every syscall with adversarial arguments: null, kernel, and overflowing pointers, zero and oversized lengths, out-of-range handles and descriptors, missing capabilities, and buffers in each ELF segment. Each check prints `PASS: <name>` or `FAIL: <name>`, and the shard ends with a summary:

```
--- Fuzz Summary ---
fuzz: pass=60 fail=0 total=60
fuzz-syscall: ALL TESTS PASSED
```

The GPU tests run last because they call `SYS_GPU_PLEDGE`, which restricts every syscall that follows.

### Adding a Check

Add a call to `check()` in the matching `test_*` function:

```c
r = coconut_fs_read(fd, (void *)0x1000, 64);
check(r == COCONUT_ERROR, "fs_read: into .text rejected");
```

A good check states the boundary it tests in its name, and fails if the kernel accepts what it should reject (or the reverse).

### Making Sure a Check Can Fail

A check that never fails proves nothing. When adding one for a kernel rule, temporarily break the rule in the supervisor and confirm the check reports `FAIL:`. For example, removing the data-segment clause from `validate_user_write_buf` in `syscall.rs` makes `fs_read: into .bss accepted` fail.

## Reading a Failure

- **`FAIL: <name>`** — the kernel returned something other than what the check expected. The lines just before it usually include the kernel's diagnostic for that syscall.
- **`PF: shard N faulted at RIP ..., addr ..., killing`** — a shard touched memory it has no mapping for, or wrote to a read-only page. Find the instruction with `llvm-objdump -d target/<shard>/<shard>.elf`. See [Debugging](debugging.md).
- **`KERNEL PANIC`** — an assertion in the supervisor failed. The panic message names the file, line, and broken invariant.
