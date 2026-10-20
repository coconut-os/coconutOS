# Writing Shards

A shard is a user-mode program running in its own address space. Shards are linked as ELF executables, embedded into the supervisor at build time, and created at boot by `supervisor_main`. This page covers what a shard can rely on and how to add a new one.

For the syscall interface, see [Syscall Reference](syscalls.md).

## Address Space

| Virtual Address | Contents | Permissions |
|-----------------|----------|-------------|
| `0x0000` | Unmapped (null guard) | |
| `0x1000` | ELF text, entry point `_start` | R+X |
| next page | ELF rodata | R+NX |
| next page | ELF data + bss (zero-filled) | R+W+NX |
| `0x4000` | GPU config page (GPU HAL shards only) | R+NX |
| `0x10000` | Pipeline config page (pipeline shards only) | R+NX |
| `image_end`..`0x7FF000` | Heap via `SYS_MMAP` | R+W+NX |
| `0x7FF000` | Stack, one 4 KiB page; `rsp` starts at `0x800000` | R+W+NX |

Things to keep in mind:

- **The stack is a single page.** Large local arrays overflow it into unmapped memory and the shard is killed with a page fault. Put big buffers in `.bss` or in mmap'd memory.
- **Globals work.** Initialized globals live in `.data`, zero-initialized ones in `.bss`. Code and constants are never writable.
- **Config pages sit at fixed addresses.** If a shard's image grows into its config page, the supervisor panics at boot (`map_4k: virt already mapped`) rather than silently replacing a page.
- **There is no libc.** C shards are built freestanding. clang may still emit calls to `memset` and `memcpy`, so each C shard defines them.

The layout is enforced by `targets/shard.ld` and checked by `shard::create` when the shard is loaded.

## C Shards

A C shard is a directory under `shards/` with two files:

- `start.S` — sets `rsp` to `0x800000`, calls `main`, and exits with its return value. Copy it from `shards/hello-c/start.S`.
- `main.c` — includes `../../include/coconut.h`, which provides inline wrappers for every syscall (`coconut_puts`, `coconut_fs_open`, `coconut_mmap`, ...).

```c
#include "../../include/coconut.h"

static char buf[256];   /* .bss, zeroed at load */

int main(void)
{
    uint64_t fd = coconut_fs_open("/hello.txt", 10);
    if (fd == COCONUT_ERROR)
        return 1;
    uint64_t n = coconut_fs_read(fd, buf, sizeof(buf));
    coconut_serial_write(buf, n);
    coconut_fs_close(fd);
    return 0;
}
```

Compile with SSE disabled (`-mno-sse -mno-sse2`) unless the shard needs floating point. The supervisor saves and restores SSE state across preemption, so float math is safe when enabled (see `shards/llama-inference`).

## Rust Shards

Rust shards are crates built for the custom target `targets/x86_64-coconut-shard.json` and use the `coconut-rt` runtime crate, which provides `_start`, `print!`/`println!`, and wrappers in `coconut_rt::sys`. See `crates/coconut-shard-gpu` for a complete example.

The custom target is soft-float. Linking uses `targets/shard.ld` through `.cargo/config.toml`.

## Adding a Shard to the Build

A new shard touches four places. Using a C shard named `example` as the running example:

1. **Build it** in `scripts/qemu-run.sh`: compile `main.c` and `start.S`, link with `rust-lld -flavor gnu -T targets/shard.ld --gc-sections`, and export the ELF path:

   ```bash
   export COCONUT_SHARD_EXAMPLE_ELF="$EXAMPLE_OBJ_DIR/example.elf"
   ```

2. **Embed it** in `crates/coconut-supervisor/build.rs`: add a block that copies `COCONUT_SHARD_EXAMPLE_ELF` to `$OUT_DIR/shard-example.elf`, following the existing ones.

3. **Create it** in `supervisor_main` (`crates/coconut-supervisor/src/main.rs`):

   ```rust
   static EXAMPLE_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shard-example.elf"));
   shard::create(EXAMPLE_ELF, "example", shard::Priority::Normal);
   ```

   Any channels or capabilities the shard needs are granted here too, with `channel::init` and `capability::grant_to_shard`.

4. **Make room.** `MAX_SHARDS` in `crates/coconut-supervisor/src/shard.rs` is currently 8 and every slot is in use, so adding a shard means raising it.

Then run `./scripts/qemu-run.sh`. If the image breaks a layout rule, the boot stops with an assertion naming the problem (for example `shard: segments not contiguous and page-aligned`).

## Priorities

`shard::Priority` has four levels: `Critical`, `High`, `Normal`, and `Low`. The scheduler always runs the highest-priority ready shard and round-robins within a level, so a `Low` shard only runs while every higher-priority shard is blocked or exited.
