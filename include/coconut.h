/*
 * coconut.h — C interface to coconutOS syscalls.
 *
 * Header-only: inline asm syscall wrappers matching the kernel's register
 * ABI (syscall.rs:100-158). No staticlib, no cross-language linking needed.
 *
 * Clobber rules: coconutOS's syscall_entry shuffles rdi/rsi/rdx into the
 * Rust dispatch function's arguments — those registers are NOT preserved.
 * Input registers use register+asm variables with "+r" constraints so GCC
 * knows they're clobbered. rcx and r11 are always clobbered by the syscall
 * instruction itself.
 */

#ifndef COCONUT_H
#define COCONUT_H

/* -----------------------------------------------------------------------
 * Fixed-width types (freestanding — no libc)
 * ----------------------------------------------------------------------- */

typedef unsigned char      uint8_t;
typedef unsigned short     uint16_t;
typedef unsigned int       uint32_t;
typedef unsigned long long uint64_t;
typedef unsigned long      size_t;

/* -----------------------------------------------------------------------
 * Syscall numbers — must match coconut-shared/src/lib.rs
 * ----------------------------------------------------------------------- */

#define SYS_EXIT          0
#define SYS_SERIAL_WRITE  1
#define SYS_CAP_GRANT    11
#define SYS_CAP_REVOKE   12
#define SYS_CAP_RESTRICT 13
#define SYS_CAP_INSPECT  14
#define SYS_CHANNEL_SEND 21
#define SYS_CHANNEL_RECV 22
#define SYS_FS_OPEN      30
#define SYS_FS_READ      31
#define SYS_FS_STAT      32
#define SYS_FS_CLOSE     33
#define SYS_GPU_DMA      40
#define SYS_GPU_PLEDGE   41
#define SYS_GPU_UNVEIL   42
#define SYS_MMAP         43
#define SYS_YIELD        62

/* Pledge bits */
#define PLEDGE_SERIAL   (1ULL << 0)
#define PLEDGE_CHANNEL  (1ULL << 1)
#define PLEDGE_GPU_DMA  (1ULL << 2)

/* Error sentinel returned by failing syscalls */
#define COCONUT_ERROR    (~0ULL)

/* -----------------------------------------------------------------------
 * GPU config page — kernel-provided partition parameters at VA 0x4000
 * ----------------------------------------------------------------------- */

#define GPU_CONFIG_VADDR  0x4000
#define GPU_CONFIG_MAGIC  0x47504346  /* "GPCF" */

struct gpu_config {
    uint32_t magic;         /* +0x00 */
    uint32_t partition_id;  /* +0x04 */
    uint64_t vram_size;     /* +0x08 */
    uint32_t cu_count;      /* +0x10 */
    uint32_t _pad;          /* +0x14 */
    uint64_t vram_vaddr;    /* +0x18 */
    uint64_t mmio_vaddr;    /* +0x20 */
};

/* -----------------------------------------------------------------------
 * Generic syscall primitives
 *
 * Register inputs use the register-asm variable pattern so GCC treats
 * them as read-write ("+r"), since the kernel clobbers rdi/rsi/rdx.
 * ----------------------------------------------------------------------- */

static inline uint64_t coconut_syscall0(uint64_t nr)
{
    register uint64_t rax __asm__("rax") = nr;
    __asm__ volatile(
        "syscall"
        : "+r"(rax)
        :
        : "rcx", "r11", "rdi", "rsi", "rdx", "r8", "r9", "r10", "memory"
    );
    return rax;
}

static inline uint64_t coconut_syscall1(uint64_t nr, uint64_t a0)
{
    register uint64_t rax __asm__("rax") = nr;
    register uint64_t rdi __asm__("rdi") = a0;
    __asm__ volatile(
        "syscall"
        : "+r"(rax), "+r"(rdi)
        :
        : "rcx", "r11", "rsi", "rdx", "r8", "r9", "r10", "memory"
    );
    return rax;
}

static inline uint64_t coconut_syscall2(uint64_t nr, uint64_t a0, uint64_t a1)
{
    register uint64_t rax __asm__("rax") = nr;
    register uint64_t rdi __asm__("rdi") = a0;
    register uint64_t rsi __asm__("rsi") = a1;
    __asm__ volatile(
        "syscall"
        : "+r"(rax), "+r"(rdi), "+r"(rsi)
        :
        : "rcx", "r11", "rdx", "r8", "r9", "r10", "memory"
    );
    return rax;
}

static inline uint64_t coconut_syscall3(uint64_t nr, uint64_t a0, uint64_t a1,
                                        uint64_t a2)
{
    register uint64_t rax __asm__("rax") = nr;
    register uint64_t rdi __asm__("rdi") = a0;
    register uint64_t rsi __asm__("rsi") = a1;
    register uint64_t rdx __asm__("rdx") = a2;
    __asm__ volatile(
        "syscall"
        : "+r"(rax), "+r"(rdi), "+r"(rsi), "+r"(rdx)
        :
        : "rcx", "r11", "r8", "r9", "r10", "memory"
    );
    return rax;
}

/* -----------------------------------------------------------------------
 * Typed syscall wrappers
 * ----------------------------------------------------------------------- */

static inline void __attribute__((noreturn)) coconut_exit(uint64_t code)
{
    register uint64_t rax __asm__("rax") = SYS_EXIT;
    register uint64_t rdi __asm__("rdi") = code;
    __asm__ volatile(
        "syscall"
        :
        : "r"(rax), "r"(rdi)
        : "rcx", "r11", "memory"
    );
    __builtin_unreachable();
}

static inline uint64_t coconut_serial_write(const void *buf, size_t len)
{
    return coconut_syscall2(SYS_SERIAL_WRITE, (uint64_t)buf, (uint64_t)len);
}

static inline void coconut_yield(void)
{
    coconut_syscall0(SYS_YIELD);
}

static inline uint64_t coconut_gpu_pledge(uint64_t mask)
{
    return coconut_syscall1(SYS_GPU_PLEDGE, mask);
}

static inline uint64_t coconut_gpu_unveil(uint64_t offset, uint64_t size)
{
    return coconut_syscall2(SYS_GPU_UNVEIL, offset, size);
}

static inline uint64_t coconut_gpu_dma(uint64_t target_partition,
                                       uint64_t src_offset,
                                       uint64_t packed_dst_len)
{
    return coconut_syscall3(SYS_GPU_DMA, target_partition, src_offset,
                            packed_dst_len);
}

static inline uint64_t coconut_channel_send(uint64_t channel, const void *buf,
                                            size_t len)
{
    return coconut_syscall3(SYS_CHANNEL_SEND, channel, (uint64_t)buf,
                            (uint64_t)len);
}

static inline uint64_t coconut_channel_recv(uint64_t channel, void *buf,
                                            size_t max_len)
{
    return coconut_syscall3(SYS_CHANNEL_RECV, channel, (uint64_t)buf,
                            (uint64_t)max_len);
}

static inline uint64_t coconut_fs_open(const char *path, size_t path_len)
{
    return coconut_syscall2(SYS_FS_OPEN, (uint64_t)path, (uint64_t)path_len);
}

static inline uint64_t coconut_fs_read(uint64_t fd, void *buf, size_t max_len)
{
    return coconut_syscall3(SYS_FS_READ, fd, (uint64_t)buf, (uint64_t)max_len);
}

static inline uint64_t coconut_fs_stat(uint64_t fd)
{
    return coconut_syscall1(SYS_FS_STAT, fd);
}

static inline uint64_t coconut_fs_close(uint64_t fd)
{
    return coconut_syscall1(SYS_FS_CLOSE, fd);
}

static inline uint64_t coconut_mmap(uint64_t va_start, uint64_t num_pages)
{
    return coconut_syscall2(SYS_MMAP, va_start, num_pages);
}

/* -----------------------------------------------------------------------
 * Convenience helpers
 * ----------------------------------------------------------------------- */

static inline size_t coconut_strlen(const char *s)
{
    size_t n = 0;
    while (s[n])
        n++;
    return n;
}

static inline uint64_t coconut_puts(const char *s)
{
    return coconut_serial_write(s, coconut_strlen(s));
}

/* Read and validate the GPU config page. Returns pointer or 0 on bad magic. */
static inline const struct gpu_config *coconut_gpu_config(void)
{
    const struct gpu_config *cfg = (const struct gpu_config *)GPU_CONFIG_VADDR;
    if (cfg->magic != GPU_CONFIG_MAGIC)
        return 0;
    return cfg;
}

#endif /* COCONUT_H */
