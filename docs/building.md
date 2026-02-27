# Building coconutOS

## Workspace Layout

coconutOS is a Cargo workspace with five crates, C shards, and three distinct build targets:

```
coconutOS/
├── Cargo.toml              # Workspace root
├── .cargo/config.toml      # Per-target rustflags
├── rust-toolchain.toml     # Nightly + components
├── mise.toml               # Task runner
├── scripts/qemu-run.sh     # Build + QEMU launcher
├── crates/
│   ├── coconut-boot/       # UEFI bootloader  (x86_64-unknown-uefi)
│   ├── coconut-supervisor/ # Microkernel       (x86_64-unknown-none)
│   ├── coconut-shared/     # Shared types      (no_std library)
│   ├── coconut-rt/         # Shard runtime     (custom shard target)
│   └── coconut-shard-gpu/  # GPU HAL shard     (custom shard target)
├── include/
│   └── coconut.h           # Header-only C interface to coconutOS syscalls
├── shards/
│   ├── hello-c/            # C FFI demo shard  (start.S + main.c)
│   └── llama-inference/    # Transformer inference shard (start.S + main.c)
└── targets/
    ├── x86_64-coconut-shard.json  # Custom target for Rust shards
    └── shard.ld            # Shard linker script (flat binary at VA 0x1000)
```

| Crate | Target | Role |
|-------|--------|------|
| `coconut-boot` | `x86_64-unknown-uefi` | UEFI application that loads the supervisor ELF |
| `coconut-supervisor` | `x86_64-unknown-none` | Freestanding microkernel, linked at higher-half VMA |
| `coconut-shared` | (library) | `repr(C)` boot handoff types and syscall constants |
| `coconut-rt` | `x86_64-coconut-shard` | Shard runtime library (syscall wrappers, GPU primitives) |
| `coconut-shard-gpu` | `x86_64-coconut-shard` | GPU HAL shard binary (Rust, `no_main`) |

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

# Build Rust GPU HAL shard (custom target)
cargo build -p coconut-shard-gpu \
    --target targets/x86_64-coconut-shard.json \
    -Zjson-target-spec --release

# Type-check both standard targets
cargo check -p coconut-supervisor --target x86_64-unknown-none
cargo check -p coconut-boot --target x86_64-unknown-uefi
```

## C Shard Build

C shards are compiled with clang targeting freestanding x86-64. The `qemu-run.sh` script handles this automatically.

```bash
# hello-c shard (SSE disabled — no float math)
clang -target x86_64-unknown-none-elf -ffreestanding -nostdlib -nostdinc \
    -mno-sse -mno-sse2 -mno-mmx -mno-red-zone -fno-stack-protector \
    -fno-pic -O2 -I include -c shards/hello-c/main.c -o hello-c.o

# llama-inference shard (SSE enabled — needs float math)
clang -target x86_64-unknown-none-elf -ffreestanding -nostdlib -nostdinc \
    -mno-mmx -mno-red-zone -fno-stack-protector -fno-pic -O2 \
    -I include -c shards/llama-inference/main.c -o llama.o

# Link with start.S entry stub, produce flat binary
clang -target x86_64-unknown-none-elf -ffreestanding -nostdlib \
    -Wl,-Ttargets/shard.ld -o shard.elf start.o main.o
llvm-objcopy -O binary shard.elf shard.bin
```

The resulting flat binaries are embedded into the supervisor via `include_bytes!` in `build.rs`. Environment variables (`COCONUT_SHARD_HELLO_C_BIN`, `COCONUT_SHARD_LLAMA_BIN`) tell the supervisor build where to find them.

## Build Artifacts

```
target/
├── x86_64-unknown-none/release/
│   ├── coconut-supervisor          # ELF binary (loaded by bootloader)
│   └── build/coconut-supervisor-*/out/
│       ├── shard-gpu-hal.bin       # GPU HAL shard (Rust, flat binary)
│       ├── shard-hello-c.bin       # C demo shard (flat binary)
│       └── shard-llama-inference.bin  # Inference shard (flat binary)
├── x86_64-unknown-uefi/release/
│   └── coconut-boot.efi           # PE32+ UEFI application
└── x86_64-coconut-shard/release/
    └── coconut-shard-gpu           # GPU HAL shard ELF (before objcopy)
```

The `qemu-run.sh` script packages the bootloader and supervisor into a FAT32 disk image at `target/coconut-boot.img`.

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
    "-C", "code-model=kernel",
    "-C", "relocation-model=static",
    "-C", "target-feature=-sse,-sse2",
    "-C", "link-arg=-Tcrates/coconut-supervisor/linker.ld",
    "-C", "link-arg=--gc-sections",
]
```

- **`code-model=kernel`**: Required because the supervisor is linked at `0xFFFFFFFF80200000` (top 2 GiB). The `kernel` code model tells LLVM that all symbols live in the upper 2 GiB, enabling efficient `mov` instructions instead of `movabs`.
- **`target-feature=-sse,-sse2`**: Prevents the Rust compiler from emitting SSE instructions in supervisor code. This is critical because user-mode shards use SSE for float math — if the supervisor clobbered XMM registers during syscall handling, shard computations would silently corrupt. FPU/SSE state is managed explicitly via FXSAVE/FXRSTOR in the timer ISR.
- **Linker script** (`crates/coconut-supervisor/linker.ld`): Implements a split VMA/LMA model — `.text.boot` runs at physical addresses while the rest of the kernel uses higher-half virtual addresses with `AT()` load addresses.

### `[target.x86_64-unknown-uefi]`

Empty `rustflags` — the UEFI target uses its default PE32+ linking.

### `[target.x86_64-coconut-shard]`

```toml
rustflags = [
    "-C", "relocation-model=static",
    "-C", "link-arg=-Ttargets/shard.ld",
    "-C", "link-arg=--gc-sections",
]
```

Rust shards use a custom target (`targets/x86_64-coconut-shard.json`) compiled into flat binaries at VA `0x1000`. The custom target requires `-Zjson-target-spec` on the cargo command line.

## Rust Toolchain (`rust-toolchain.toml`)

```toml
[toolchain]
channel = "nightly"
components = ["rust-src", "llvm-tools-preview"]
targets = ["x86_64-unknown-uefi", "x86_64-unknown-none"]
```

Nightly is required for `build-std`, `#[unsafe(naked)]`, and `naked_asm!()`. The `llvm-tools-preview` component provides `rust-objdump` and `llvm-objcopy` for disassembly and binary conversion.
