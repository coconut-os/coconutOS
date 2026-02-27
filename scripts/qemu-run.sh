#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="$ROOT_DIR/target"
IMG="$TARGET_DIR/coconut-boot.img"

# OVMF firmware paths (Homebrew)
OVMF_CODE="${OVMF_CODE:-}"
if [ -z "$OVMF_CODE" ]; then
    for path in \
        /opt/homebrew/share/qemu/edk2-x86_64-code.fd \
        /usr/local/share/qemu/edk2-x86_64-code.fd \
        /usr/share/OVMF/OVMF_CODE.fd \
        /usr/share/edk2/ovmf/OVMF_CODE.fd; do
        if [ -f "$path" ]; then
            OVMF_CODE="$path"
            break
        fi
    done
fi

if [ -z "$OVMF_CODE" ]; then
    echo "ERROR: OVMF firmware not found. Install QEMU (brew install qemu) or set OVMF_CODE."
    exit 1
fi

# Check dependencies
for cmd in qemu-system-x86_64 mformat mcopy clang; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "ERROR: $cmd not found. Install with: brew install qemu mtools llvm"
        exit 1
    fi
done

echo "==> Building coconut-shard-gpu (release)..."
cargo build -p coconut-shard-gpu --target "$ROOT_DIR/targets/x86_64-coconut-shard.json" --release \
    --manifest-path "$ROOT_DIR/Cargo.toml" -Zjson-target-spec

SHARD_ELF="$TARGET_DIR/x86_64-coconut-shard/release/coconut-shard-gpu"
if [ ! -f "$SHARD_ELF" ]; then
    echo "ERROR: Shard ELF not found at $SHARD_ELF"
    exit 1
fi

# Convert ELF to flat binary for embedding in the supervisor
LLVM_OBJCOPY="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's|host: ||p')/bin/llvm-objcopy"
if [ ! -f "$LLVM_OBJCOPY" ]; then
    # Fallback to PATH
    LLVM_OBJCOPY="llvm-objcopy"
fi
"$LLVM_OBJCOPY" -O binary "$SHARD_ELF" "$TARGET_DIR/shard-gpu.bin"

export COCONUT_SHARD_GPU_BIN="$TARGET_DIR/shard-gpu.bin"

# Locate rust-lld from the Rust toolchain (same sysroot as llvm-objcopy)
RUST_LLD="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's|host: ||p')/bin/rust-lld"
if [ ! -f "$RUST_LLD" ]; then
    RUST_LLD="rust-lld"
fi

echo "==> Building hello-c shard..."
HELLO_C_DIR="$ROOT_DIR/shards/hello-c"
HELLO_C_OBJ_DIR="$TARGET_DIR/hello-c"
mkdir -p "$HELLO_C_OBJ_DIR"

clang -target x86_64-unknown-none-elf -ffreestanding -nostdlib -nostdinc \
    -mno-sse -mno-sse2 -mno-mmx -mno-red-zone -fno-stack-protector -fno-pic \
    -O2 -c "$HELLO_C_DIR/main.c" -o "$HELLO_C_OBJ_DIR/main.o"

clang -target x86_64-unknown-none-elf -c "$HELLO_C_DIR/start.S" \
    -o "$HELLO_C_OBJ_DIR/start.o"

"$RUST_LLD" -flavor gnu -T "$ROOT_DIR/targets/shard.ld" --gc-sections \
    "$HELLO_C_OBJ_DIR/start.o" "$HELLO_C_OBJ_DIR/main.o" \
    -o "$HELLO_C_OBJ_DIR/hello-c.elf"

"$LLVM_OBJCOPY" -O binary "$HELLO_C_OBJ_DIR/hello-c.elf" \
    "$TARGET_DIR/shard-hello-c.bin"

export COCONUT_SHARD_HELLO_C_BIN="$TARGET_DIR/shard-hello-c.bin"

echo "==> Building coconut-supervisor (release)..."
cargo build -p coconut-supervisor --target x86_64-unknown-none --release \
    --manifest-path "$ROOT_DIR/Cargo.toml"

echo "==> Building coconut-boot (release)..."
cargo build -p coconut-boot --target x86_64-unknown-uefi --release \
    --manifest-path "$ROOT_DIR/Cargo.toml"

BOOTLOADER="$TARGET_DIR/x86_64-unknown-uefi/release/coconut-boot.efi"
SUPERVISOR="$TARGET_DIR/x86_64-unknown-none/release/coconut-supervisor"

if [ ! -f "$BOOTLOADER" ]; then
    echo "ERROR: Bootloader not found at $BOOTLOADER"
    exit 1
fi
if [ ! -f "$SUPERVISOR" ]; then
    echo "ERROR: Supervisor not found at $SUPERVISOR"
    exit 1
fi

echo "==> Creating FAT32 boot image..."
# Create a 64 MiB FAT32 image
dd if=/dev/zero of="$IMG" bs=1M count=64 status=none
mformat -i "$IMG" -F ::

# Create directory structure and copy files
mmd -i "$IMG" ::/EFI
mmd -i "$IMG" ::/EFI/BOOT
mmd -i "$IMG" ::/EFI/coconut
mcopy -i "$IMG" "$BOOTLOADER" ::/EFI/BOOT/BOOTX64.EFI
mcopy -i "$IMG" "$SUPERVISOR" ::/EFI/coconut/supervisor.elf

echo "==> Launching QEMU..."
echo "    OVMF: $OVMF_CODE"
echo "    Image: $IMG"
echo ""

qemu-system-x86_64 \
    -machine q35,kernel-irqchip=split \
    -device intel-iommu,intremap=on \
    -m 128M \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive format=raw,file="$IMG" \
    -serial stdio \
    -display none \
    -no-reboot \
    -no-shutdown \
    "$@"
