# Building coconutOS

## Workspace Layout

coconutOS is a Cargo workspace with three crates and two distinct build targets:

```
coconutOS/
├── Cargo.toml              # Workspace root
├── .cargo/config.toml      # Per-target rustflags
├── rust-toolchain.toml     # Nightly + components
├── mise.toml               # Task runner
├── scripts/qemu-run.sh     # Build + QEMU launcher
└── crates/
    ├── coconut-boot/       # UEFI bootloader  (x86_64-unknown-uefi)
    ├── coconut-supervisor/  # Microkernel      (x86_64-unknown-none)
    └── coconut-shared/     # Shared types      (no_std library)
```

| Crate | Target | Role |
|-------|--------|------|
| `coconut-boot` | `x86_64-unknown-uefi` | UEFI application that loads the supervisor ELF |
| `coconut-supervisor` | `x86_64-unknown-none` | Freestanding microkernel, linked at higher-half VMA |
| `coconut-shared` | (library) | `repr(C)` boot handoff types and syscall constants |

## mise Tasks

If you have [mise](https://mise.jdx.dev/) installed:

| Task | Command | What it does |
|------|---------|--------------|
| `mise run build` | `cargo build -p coconut-supervisor --target x86_64-unknown-none --release` | Build supervisor |
| `mise run build-boot` | `cargo build -p coconut-boot --target x86_64-unknown-uefi --release` | Build bootloader |
| `mise run build-all` | Runs `build` then `build-boot` | Build everything |
| `mise run run` | `./scripts/qemu-run.sh` | Build + boot QEMU |
| `mise run check` | `cargo check` both targets | Type-check without linking |
| `mise run fmt` | `cargo fmt` | Format code |
| `mise run fmt-check` | `cargo fmt --check` | CI format check |
| `mise run clean` | `cargo clean` | Remove build artifacts |
| `mise run objdump` | `rust-objdump` on supervisor binary | Disassemble for debugging |

## Raw Cargo Commands

```bash
# Build supervisor (freestanding kernel)
cargo build -p coconut-supervisor --target x86_64-unknown-none --release

# Build bootloader (UEFI application)
cargo build -p coconut-boot --target x86_64-unknown-uefi --release

# Type-check both
cargo check -p coconut-supervisor --target x86_64-unknown-none
cargo check -p coconut-boot --target x86_64-unknown-uefi
```

## Build Artifacts

```
target/
├── x86_64-unknown-none/release/
│   └── coconut-supervisor          # ELF binary (loaded by bootloader)
└── x86_64-unknown-uefi/release/
    └── coconut-boot.efi            # PE32+ UEFI application
```

The `qemu-run.sh` script packages both into a FAT32 disk image at `target/coconut-boot.img`.

## Cargo Configuration (`.cargo/config.toml`)

### `[unstable]`

```toml
build-std = ["core", "alloc", "compiler_builtins"]
build-std-features = ["compiler-builtins-mem"]
```

Both targets are freestanding — there is no `std`. Cargo builds `core` and `alloc` from source using the `rust-src` component.

### `[target.x86_64-unknown-none]`

```toml
rustflags = [
    "-C", "code-model=kernel",         # All symbols in top 2 GiB
    "-C", "relocation-model=static",   # No PIC/PIE
    "-C", "link-arg=-Tcrates/coconut-supervisor/linker.ld",
    "-C", "link-arg=--gc-sections",
]
```

- **`code-model=kernel`**: Required because the supervisor is linked at `0xFFFFFFFF80200000` (top 2 GiB). The `kernel` code model tells LLVM that all symbols live in the upper 2 GiB, enabling efficient `mov` instructions instead of `movabs`.
- **Linker script** (`crates/coconut-supervisor/linker.ld`): Implements a split VMA/LMA model — `.text.boot` runs at physical addresses while the rest of the kernel uses higher-half virtual addresses with `AT()` load addresses.

### `[target.x86_64-unknown-uefi]`

Empty `rustflags` — the UEFI target uses its default PE32+ linking.

## Rust Toolchain (`rust-toolchain.toml`)

```toml
[toolchain]
channel = "nightly"
components = ["rust-src", "llvm-tools-preview"]
targets = ["x86_64-unknown-uefi", "x86_64-unknown-none"]
```

Nightly is required for `build-std`, `#[unsafe(naked)]`, and `naked_asm!()`. The `llvm-tools-preview` component provides `rust-objdump` for disassembly.
