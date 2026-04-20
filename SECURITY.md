# Security Policy

coconutOS is a security-focused microkernel. Isolation guarantees are a core design goal, not a feature — bugs in the syscall boundary, capability system, or GPU isolation are treated with the same severity as data loss.

## Reporting a Vulnerability

If you find a security issue in coconutOS, please report it privately:

1. **Email:** security@raskell.io
2. **GitHub:** Use [Security Advisories](https://github.com/coconut-os/coconutOS/security/advisories/new) to report privately.

Please include:
- Which syscall, capability, or isolation boundary is affected
- A minimal reproduction (a shard binary or syscall sequence that triggers the issue)
- Whether it causes a kernel panic, information leak, privilege escalation, or isolation bypass

## Response

- Acknowledgment within 48 hours
- Fix or mitigation within 7 days for confirmed issues
- Credit in the commit message unless you prefer anonymity

## Scope

The following are in scope:

- **Syscall boundary:** Buffer validation bypass, missing bounds checks, panics from user input
- **Capability system:** Forgery, escalation, revocation bypass
- **GPU isolation:** IOMMU bypass, cross-partition VRAM access, DMA without capability
- **Shard isolation:** Page table escapes, kernel memory reads, side-channel leaks
- **Scheduler:** Priority inversion, starvation, state corruption

Out of scope:

- Denial of service via excessive syscalls (shards are preemptively scheduled)
- Issues in the QEMU emulation layer
- Build system or tooling bugs

## Track Record

coconutOS includes a built-in [fuzz shard](shards/fuzz-syscall/) that exercises all syscall handlers with adversarial inputs on every boot. Two security bugs have been found and fixed through internal fuzzing:

- **User-mode page fault kernel panic** — #PF handler did not check CS RPL, allowing a malicious shard to crash the supervisor by accessing unmapped memory. Fixed in `c4915f1`.
- **mmap gap validation bypass** — Non-contiguous mmap calls created gaps in the data region where buffer validation passed but pages were absent. Fixed in `c4915f1`.
