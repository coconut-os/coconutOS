# coconutOS

Rust microkernel for GPU-isolated AI inference. Three-crate workspace: `coconut-boot` (UEFI bootloader), `coconut-supervisor` (freestanding kernel), `coconut-shared` (ABI types + constants).

## Philosophy

This project follows OpenBSD's approach to systems software: **correct, minimal, readable, and secure**. Every line of code must justify its existence. Every comment must explain *why*, not *what*. Every feature must be required — not convenient, not "nice to have", not "in case we need it later".

Less is more. Simple is better than clever. Readable is better than fast (until profiling proves otherwise).

## Build and Run

```bash
./scripts/qemu-run.sh     # builds both crates, boots in QEMU
```

Nightly Rust. Targets: `x86_64-unknown-uefi` (boot), `x86_64-unknown-none` (supervisor). The toolchain is pinned in `rust-toolchain.toml`.

## Key References

- [.claude/ROADMAP.md](ROADMAP.md) — milestone tracker
- [docs/architecture.md](../docs/architecture.md) — full system design
- [docs/getting-started.md](../docs/getting-started.md) — prerequisites, build, expected output
- [docs/building.md](../docs/building.md) — workspace layout, cargo config
- [docs/debugging.md](../docs/debugging.md) — GDB, serial, common faults

## Rules

Development rules are in `.claude/rules/`:

- [code-style.md](rules/code-style.md) — formatting, naming, comments
- [architecture.md](rules/architecture.md) — module structure, dependencies, crate boundaries
- [safety.md](rules/safety.md) — unsafe code, security, correctness
- [minimalism.md](rules/minimalism.md) — feature discipline, less-is-more
- [commits.md](rules/commits.md) — git workflow, commit messages
