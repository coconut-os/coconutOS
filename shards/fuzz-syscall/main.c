/*
 * fuzz-syscall — syscall boundary torture test.
 *
 * Exercises all syscall handlers with adversarial inputs: invalid pointers,
 * boundary values, overflow attempts, capability violations. Each test
 * must return gracefully (u64::MAX error or correct result) without
 * crashing the supervisor.
 *
 * Output format: "PASS: <test>" or "FAIL: <test>" via serial.
 * Exit code: number of failures.
 */

#include "../../include/coconut.h"

/* clang -ffreestanding may emit implicit calls to memset/memcpy */

void *memset(void *s, int c, size_t n)
{
    uint8_t *p = (uint8_t *)s;
    while (n--)
        *p++ = (uint8_t)c;
    return s;
}

void *memcpy(void *dest, const void *src, size_t n)
{
    uint8_t *d = (uint8_t *)dest;
    const uint8_t *s = (const uint8_t *)src;
    while (n--)
        *d++ = *s++;
    return dest;
}

/* Size of the supervisor's shared open-file table (fs.rs) */
#define MAX_OPEN_FILES 16

/*
 * supervisor_main grants this shard CAP_CHANNEL(FUZZ_CHANNEL) with
 * SEND | GRANT in slot 0. Shard IDs follow creation order there.
 */
#define FUZZ_CHANNEL     2
#define FS_READER_SHARD  2
#define FUZZ_SHARD       7

/* -----------------------------------------------------------------------
 * Test infrastructure
 * ----------------------------------------------------------------------- */

static int pass_count;
static int fail_count;

/*
 * Output lines are assembled in a buffer and written with one syscall.
 * Syscalls are never preempted, so kernel messages cannot land in the
 * middle of a line (the campaign seed must stay readable).
 */
struct line {
    char buf[160];
    size_t len;
};

static void line_str(struct line *l, const char *s)
{
    while (*s && l->len < sizeof(l->buf))
        l->buf[l->len++] = *s++;
}

static void line_u64(struct line *l, uint64_t v)
{
    char digits[20];
    int n = 0;
    do {
        digits[n++] = '0' + v % 10;
        v /= 10;
    } while (v > 0);
    while (n > 0 && l->len < sizeof(l->buf))
        l->buf[l->len++] = digits[--n];
}

static void line_end(struct line *l)
{
    line_str(l, "\n");
    coconut_serial_write(l->buf, l->len);
}

static void check(int condition, const char *name)
{
    struct line l = { .len = 0 };
    if (condition) {
        line_str(&l, "PASS: ");
        pass_count++;
    } else {
        line_str(&l, "FAIL: ");
        fail_count++;
    }
    line_str(&l, name);
    line_end(&l);
}

/* -----------------------------------------------------------------------
 * SYS_SERIAL_WRITE tests
 * ----------------------------------------------------------------------- */

static void test_serial_write(void)
{
    /* Valid write from code region */
    uint64_t r = coconut_serial_write("ok", 2);
    coconut_puts("\n");
    check(r == 0, "serial_write: valid buf in code");

    /* Zero length */
    r = coconut_serial_write("x", 0);
    check(r == COCONUT_ERROR, "serial_write: len=0 rejected");

    /* Length exceeds limit (4096) */
    r = coconut_syscall2(SYS_SERIAL_WRITE, 0x1000, 4097);
    check(r == COCONUT_ERROR, "serial_write: len=4097 rejected");

    /* Null pointer */
    r = coconut_syscall2(SYS_SERIAL_WRITE, 0, 1);
    check(r == COCONUT_ERROR, "serial_write: ptr=0 rejected");

    /* Pointer in kernel space */
    r = coconut_syscall2(SYS_SERIAL_WRITE, 0xFFFFFFFF80000000ULL, 1);
    check(r == COCONUT_ERROR, "serial_write: kernel ptr rejected");

    /* Pointer wrap-around */
    r = coconut_syscall2(SYS_SERIAL_WRITE, 0xFFFFFFFFFFFFFFFFULL, 2);
    check(r == COCONUT_ERROR, "serial_write: ptr overflow rejected");

    /* Pointer just past stack top */
    r = coconut_syscall2(SYS_SERIAL_WRITE, 0x800000, 1);
    check(r == COCONUT_ERROR, "serial_write: past stack top rejected");

    /* Valid write from stack region */
    char stack_buf[4] = "stk";
    r = coconut_serial_write(stack_buf, 3);
    coconut_puts("\n");
    check(r == 0, "serial_write: valid buf on stack");
}

/* Open /hello.txt until the table is full, close everything, return the count */
static int free_descriptors(void)
{
    uint64_t fds[MAX_OPEN_FILES + 1];
    int opened = 0;
    while (opened <= MAX_OPEN_FILES) {
        fds[opened] = coconut_fs_open("/hello.txt", 10);
        if (fds[opened] == COCONUT_ERROR)
            break;
        opened++;
    }
    for (int i = 0; i < opened; i++)
        coconut_fs_close(fds[i]);
    return opened;
}

/* -----------------------------------------------------------------------
 * SYS_FS_* tests
 * ----------------------------------------------------------------------- */

static void test_filesystem(void)
{
    /* Open valid file */
    uint64_t fd = coconut_fs_open("/hello.txt", 10);
    check(fd != COCONUT_ERROR, "fs_open: valid file");

    /* Stat */
    uint64_t size = coconut_fs_stat(fd);
    check(size != COCONUT_ERROR && size > 0, "fs_stat: returns size");

    /* Read */
    char buf[64];
    uint64_t r = coconut_fs_read(fd, buf, 64);
    check(r != COCONUT_ERROR && r > 0, "fs_read: returns bytes");

    /* Close */
    r = coconut_fs_close(fd);
    check(r == 0, "fs_close: success");

    /* Read after close */
    r = coconut_fs_read(fd, buf, 64);
    check(r == COCONUT_ERROR, "fs_read: closed fd rejected");

    /* Open nonexistent */
    r = coconut_fs_open("/nope.txt", 8);
    check(r == COCONUT_ERROR, "fs_open: nonexistent rejected");

    /* Open with zero length path */
    r = coconut_syscall2(SYS_FS_OPEN, 0x1000, 0);
    check(r == COCONUT_ERROR, "fs_open: path_len=0 rejected");

    /* Open with invalid pointer */
    r = coconut_syscall2(SYS_FS_OPEN, 0, 5);
    check(r == COCONUT_ERROR, "fs_open: null path rejected");

    /* Stat on invalid fd */
    r = coconut_fs_stat(999);
    check(r == COCONUT_ERROR, "fs_stat: bad fd rejected");

    /* Close on invalid fd */
    r = coconut_fs_close(999);
    check(r == COCONUT_ERROR, "fs_close: bad fd rejected");

    /* Read with invalid buf ptr */
    fd = coconut_fs_open("/hello.txt", 10);
    r = coconut_syscall3(SYS_FS_READ, fd, 0, 10);
    check(r == COCONUT_ERROR, "fs_read: null buf rejected");
    coconut_fs_close(fd);

    /* Read with zero length */
    fd = coconut_fs_open("/hello.txt", 10);
    r = coconut_syscall3(SYS_FS_READ, fd, (uint64_t)buf, 0);
    check(r == COCONUT_ERROR, "fs_read: len=0 rejected");
    coconut_fs_close(fd);

    /*
     * The open-file table is shared by all shards. This shard runs at Low
     * priority, after the others have exited or blocked, so every entry
     * should be free: anything else is a descriptor leaked by the kernel.
     */
    check(free_descriptors() == MAX_OPEN_FILES, "fs_open: all 16 descriptors available");
}

/* -----------------------------------------------------------------------
 * SYS_MMAP tests
 * ----------------------------------------------------------------------- */

static void test_mmap(void)
{
    /* Valid mmap — allocate heap */
    uint64_t r = coconut_mmap(0x100000, 4);
    check(r == 0, "mmap: valid allocation");

    /* Write and read back to verify pages are mapped */
    volatile uint32_t *p = (volatile uint32_t *)0x100000;
    *p = 0xDEADBEEF;
    check(*p == 0xDEADBEEF, "mmap: read-write works");

    /* Not page-aligned */
    r = coconut_mmap(0x100001, 1);
    check(r == COCONUT_ERROR, "mmap: unaligned rejected");

    /* Zero pages */
    r = coconut_mmap(0x200000, 0);
    check(r == COCONUT_ERROR, "mmap: zero pages rejected");

    /* Too many pages */
    r = coconut_mmap(0x200000, 257);
    check(r == COCONUT_ERROR, "mmap: 257 pages rejected");

    /* Overlaps code region */
    r = coconut_mmap(0x1000, 1);
    check(r == COCONUT_ERROR, "mmap: overlap code rejected");

    /* Overlaps stack */
    r = coconut_mmap(0x7FF000, 1);
    check(r == COCONUT_ERROR, "mmap: overlap stack rejected");

    /* Non-contiguous (gap between existing data region) */
    r = coconut_mmap(0x200000, 1);
    check(r == COCONUT_ERROR, "mmap: non-contiguous rejected");

    /* Contiguous extension (immediately after existing) */
    r = coconut_mmap(0x104000, 1);
    check(r == 0, "mmap: contiguous extension ok");

    /* Overflow: va_start + size wraps */
    r = coconut_mmap(0xFFFFFFFFF000ULL, 2);
    check(r == COCONUT_ERROR, "mmap: overflow rejected");
}

/* -----------------------------------------------------------------------
 * SYS_CAP_* tests
 * ----------------------------------------------------------------------- */

static void test_capabilities(void)
{
    /* The channel capability granted at boot */
    uint64_t held = ((uint64_t)CAP_CHANNEL << 48) | ((uint64_t)FUZZ_CHANNEL << 16) |
                    RIGHT_CHANNEL_SEND | RIGHT_CHANNEL_GRANT;
    uint64_t r = coconut_syscall1(SYS_CAP_INSPECT, 0);
    check(r == held, "cap_inspect: boot-granted channel cap");

    /* Inspect on empty slot */
    r = coconut_syscall1(SYS_CAP_INSPECT, 15);
    check(r == COCONUT_ERROR, "cap_inspect: empty slot returns error");

    /* Inspect on out-of-bounds handle */
    r = coconut_syscall1(SYS_CAP_INSPECT, 99);
    check(r == COCONUT_ERROR, "cap_inspect: handle=99 rejected");

    /* Revoke on empty slot */
    r = coconut_syscall1(SYS_CAP_REVOKE, 15);
    check(r == COCONUT_ERROR, "cap_revoke: empty slot returns error");

    /* Restrict on empty slot */
    r = coconut_syscall2(SYS_CAP_RESTRICT, 15, 0);
    check(r == COCONUT_ERROR, "cap_restrict: empty slot returns error");

    /* Grant to invalid shard */
    r = coconut_syscall3(SYS_CAP_GRANT, 0, 255, 0xFFFF);
    check(r == COCONUT_ERROR, "cap_grant: target_shard=255 rejected");

    /* Grant from empty handle */
    r = coconut_syscall3(SYS_CAP_GRANT, 15, FUZZ_SHARD, 0xFFFF);
    check(r == COCONUT_ERROR, "cap_grant: empty handle rejected");

    /* Grant a copy to ourselves; rights are the intersection (SEND only) */
    uint64_t copy = coconut_syscall3(SYS_CAP_GRANT, 0, FUZZ_SHARD, RIGHT_CHANNEL_SEND);
    check(copy != COCONUT_ERROR && copy != 0, "cap_grant: copy to self");
    uint64_t expect = ((uint64_t)CAP_CHANNEL << 48) | ((uint64_t)FUZZ_CHANNEL << 16) |
                      RIGHT_CHANNEL_SEND;
    check(coconut_syscall1(SYS_CAP_INSPECT, copy) == expect, "cap_grant: copy has requested rights only");

    /* Restrict is monotonic: rights removed cannot be restored */
    r = coconut_syscall2(SYS_CAP_RESTRICT, copy, 0);
    check(r == 0, "cap_restrict: drop all rights");
    coconut_syscall2(SYS_CAP_RESTRICT, copy, 0xFFFF);
    check((coconut_syscall1(SYS_CAP_INSPECT, copy) & 0xFFFF) == 0, "cap_restrict: cannot add rights");

    /* A copy without GRANT cannot be granted onwards */
    r = coconut_syscall3(SYS_CAP_GRANT, copy, FUZZ_SHARD, 0xFFFF);
    check(r == COCONUT_ERROR, "cap_grant: source without GRANT rejected");

    /* Revoke the copy */
    r = coconut_syscall1(SYS_CAP_REVOKE, copy);
    check(r == 0, "cap_revoke: held cap");
    check(coconut_syscall1(SYS_CAP_INSPECT, copy) == COCONUT_ERROR, "cap_revoke: slot is empty after revoke");
}

/* -----------------------------------------------------------------------
 * SYS_CHANNEL_* tests
 * ----------------------------------------------------------------------- */

static void test_channels(void)
{
    char buf[16] = "test";

    /* Send on channel we don't own */
    uint64_t r = coconut_channel_send(0, buf, 4);
    check(r == COCONUT_ERROR, "channel_send: no cap rejected");

    /* Send on out-of-bounds channel */
    r = coconut_syscall3(SYS_CHANNEL_SEND, 99, (uint64_t)buf, 4);
    check(r == COCONUT_ERROR, "channel_send: channel=99 rejected");

    /* Send with zero length */
    r = coconut_syscall3(SYS_CHANNEL_SEND, 0, (uint64_t)buf, 0);
    check(r == COCONUT_ERROR, "channel_send: len=0 rejected");

    /* Send with length > 256 */
    r = coconut_syscall3(SYS_CHANNEL_SEND, 0, (uint64_t)buf, 257);
    check(r == COCONUT_ERROR, "channel_send: len=257 rejected");

    /* Send with invalid buffer */
    r = coconut_syscall3(SYS_CHANNEL_SEND, 0, 0, 4);
    check(r == COCONUT_ERROR, "channel_send: null buf rejected");

    /* Recv on channel we don't own */
    r = coconut_channel_recv(0, buf, 16);
    check(r == COCONUT_ERROR, "channel_recv: no cap rejected");

    /* Send on the channel we hold SEND for */
    r = coconut_channel_send(FUZZ_CHANNEL, buf, 4);
    check(r == 0, "channel_send: held cap accepted");

    /* We hold no RECV right, so this must fail instead of blocking */
    r = coconut_channel_recv(FUZZ_CHANNEL, buf, 16);
    check(r == COCONUT_ERROR, "channel_recv: SEND-only cap rejected");

    /* Capability resources are 32-bit: the high bits must not alias channel 2 */
    r = coconut_syscall3(SYS_CHANNEL_SEND, (1ULL << 32) | FUZZ_CHANNEL, (uint64_t)buf, 4);
    check(r == COCONUT_ERROR, "channel_send: channel id above 32 bits rejected");
}

/* -----------------------------------------------------------------------
 * SYS_GPU_* tests
 * ----------------------------------------------------------------------- */

static void test_gpu(void)
{
    /* DMA without owning a partition */
    uint64_t r = coconut_gpu_dma(0, 0, (0ULL << 32) | 64);
    check(r == COCONUT_ERROR, "gpu_dma: no partition rejected");

    /* DMA to invalid partition */
    r = coconut_gpu_dma(99, 0, (0ULL << 32) | 64);
    check(r == COCONUT_ERROR, "gpu_dma: partition=99 rejected");

    /* Pledge — should succeed (monotonic AND with unrestricted) */
    r = coconut_gpu_pledge(PLEDGE_SERIAL | PLEDGE_PERF);
    check(r == 0, "gpu_pledge: restrict to serial+perf");

    /* After pledge, channel send should be denied */
    char buf[4] = "x";
    r = coconut_syscall3(SYS_CHANNEL_SEND, 0, (uint64_t)buf, 1);
    check(r == COCONUT_ERROR, "pledge: channel_send denied after pledge");

    /* Serial should still work */
    r = coconut_serial_write(".", 1);
    check(r == 0, "pledge: serial_write still allowed");

    /* Perf counter should still work */
    r = coconut_perf_counter();
    check(r != COCONUT_ERROR, "pledge: perf_counter still allowed");

    /* Unveil without a partition */
    r = coconut_gpu_unveil(0, 4096);
    check(r == COCONUT_ERROR, "gpu_unveil: no partition rejected");
}

/* -----------------------------------------------------------------------
 * ELF image segment tests
 * ----------------------------------------------------------------------- */

/* volatile: force real loads and stores so the checks hit memory */
static volatile int data_word = 42;
static volatile char bss_buf[64];
static const char rodata_buf[64] = "read-only";

static void test_image_segments(void)
{
    check(data_word == 42, "image: .data initialized");
    data_word = 7;
    check(data_word == 7, "image: .data writable");

    int zeroed = 1;
    for (int i = 0; i < 64; i++) {
        if (bss_buf[i] != 0)
            zeroed = 0;
    }
    check(zeroed, "image: .bss zeroed");

    /* Syscalls may write into .data/.bss, but never into .rodata or .text */
    uint64_t fd = coconut_fs_open("/hello.txt", 10);
    uint64_t r = coconut_fs_read(fd, (void *)bss_buf, 64);
    check(r != COCONUT_ERROR && r > 0, "fs_read: into .bss accepted");

    r = coconut_fs_read(fd, (void *)rodata_buf, 64);
    check(r == COCONUT_ERROR, "fs_read: into .rodata rejected");

    r = coconut_fs_read(fd, (void *)0x1000, 64);
    check(r == COCONUT_ERROR, "fs_read: into .text rejected");
    coconut_fs_close(fd);

    /* The image's data segment is not available to mmap */
    r = coconut_mmap((uint64_t)&data_word & ~0xFFFULL, 1);
    check(r == COCONUT_ERROR, "mmap: overlap .data rejected");
}

/* -----------------------------------------------------------------------
 * SYS_PERF_COUNTER tests
 * ----------------------------------------------------------------------- */

static void test_perf_counter(void)
{
    uint64_t t0 = coconut_perf_counter();
    check(t0 != COCONUT_ERROR, "perf_counter: returns value");

    uint64_t t1 = coconut_perf_counter();
    check(t1 >= t0, "perf_counter: monotonic");
}

/* -----------------------------------------------------------------------
 * Unknown syscall test
 * ----------------------------------------------------------------------- */

static void test_unknown_syscall(void)
{
    /* Syscall number that doesn't exist */
    uint64_t r = coconut_syscall0(9999);
    check(r == COCONUT_ERROR, "unknown syscall 9999 rejected");

    r = coconut_syscall0(63);
    check(r == COCONUT_ERROR, "unknown syscall 63 rejected");
}

/* -----------------------------------------------------------------------
 * Random syscall campaign
 *
 * syzkaller-style: each iteration picks a syscall and builds its arguments
 * from typed generators that favour boundary values (region edges, length
 * limits, out-of-range handles, 32-bit truncation) over uniform noise.
 * Descriptors returned by SYS_FS_OPEN are remembered and reused as arguments.
 *
 * The oracle is that nothing breaks: no kernel panic, this shard is never
 * killed, its text and rodata are unchanged, and no descriptor leaks.
 * The seed is printed so a failing run can be replayed exactly:
 *   FUZZ_SEED=<seed> ./scripts/qemu-run.sh
 * ----------------------------------------------------------------------- */

#ifndef FUZZ_SEED
#define FUZZ_SEED 0     /* 0 = seed from the TSC at startup */
#endif

#ifndef FUZZ_ITERS
#define FUZZ_ITERS 2000
#endif

/* Base of the heap mapped by test_mmap */
#define HEAP_BASE 0x100000ULL

/* shard.ld: first byte after text and rodata */
extern const char __image_rw_start[];

static uint64_t rng_state;

/* xorshift64* */
static uint64_t rand64(void)
{
    rng_state ^= rng_state >> 12;
    rng_state ^= rng_state << 25;
    rng_state ^= rng_state >> 27;
    return rng_state * 0x2545F4914F6CDD1DULL;
}

static uint64_t below(uint64_t n)
{
    return rand64() % n;
}

/*
 * Buffers the kernel may read from or write to. Each is two pages and
 * pointers are drawn from the first page, so any length the kernel
 * accepts (at most 4096) stays inside the buffer instead of clobbering
 * this shard's own state. `quiet` is never written and holds only '.',
 * so SYS_SERIAL_WRITE echoes nothing that could look like a test result.
 */
static char scratch[2 * 4096] __attribute__((aligned(4096)));
static char quiet[2 * 4096] __attribute__((aligned(4096)));

static uint64_t open_fds[MAX_OPEN_FILES];
static int open_fd_count;
static int iteration;

/* Pointers that must never be accepted, plus region edges */
static uint64_t hostile_ptr(void)
{
    switch (below(10)) {
    case 0: return 0;
    case 1: return 0xFFF;                       /* last byte of null page */
    case 2: return 0x200000;                    /* unmapped hole */
    case 3: return 0x7FF000 - 1;                /* just below the stack */
    case 4: return 0x800000;                    /* stack top */
    case 5: return 0x0000800000000000ULL;       /* first non-canonical */
    case 6: return 0xFFFF800000000000ULL;       /* kernel HHDM */
    case 7: return 0xFFFFFFFF80000000ULL;       /* kernel text */
    case 8: return ~0ULL - below(4096);         /* wraps on any length */
    default: return rand64();
    }
}

/* Source buffer for SYS_SERIAL_WRITE: echoed to the console */
static uint64_t echo_ptr(void)
{
    if (below(2))
        return (uint64_t)quiet + below(4096);
    return hostile_ptr();
}

/* Source buffer for syscalls that read user memory without echoing it */
static uint64_t read_ptr(void)
{
    switch (below(5)) {
    case 0: return (uint64_t)quiet + below(4096);
    case 1: return 0x1000 + below(64);          /* text */
    case 2: return (uint64_t)__image_rw_start - 1 - below(64); /* rodata end */
    case 3: return HEAP_BASE + below(4096);
    default: return hostile_ptr();
    }
}

/* Destination buffer for syscalls that write user memory */
static uint64_t write_ptr(void)
{
    switch (below(5)) {
    case 0: return (uint64_t)scratch + below(4096);
    case 1: return HEAP_BASE + below(4096);
    case 2: return 0x1000 + below(64);          /* text: must be rejected */
    case 3: return (uint64_t)__image_rw_start - 1 - below(64); /* rodata */
    default: return hostile_ptr();
    }
}

static uint64_t pick_len(void)
{
    static const uint64_t edges[] = {
        0, 1, 2, 255, 256, 257, 4095, 4096, 4097,
        0xFFFFFFFFULL, 0x100000000ULL, ~0ULL,
    };
    switch (below(4)) {
    case 0: return below(4097);
    case 1: return rand64();
    default: return edges[below(sizeof(edges) / sizeof(edges[0]))];
    }
}

/* Handles, fds, shard IDs, channel and partition IDs */
static uint64_t pick_index(uint64_t limit)
{
    switch (below(6)) {
    case 0: return limit - 1;
    case 1: return limit;
    case 2: return rand64();
    case 3: return (1ULL << 32) | below(limit); /* truncated to 32 bits? */
    default: return below(limit);
    }
}

/*
 * Handle for revoke and restrict. Slot 0 holds the boot-granted channel
 * capability that every grant and send depends on, so it is spared until
 * the last tenth of the campaign; losing it early ends that coverage.
 */
static uint64_t pick_destructive_handle(void)
{
    uint64_t h = pick_index(16);
    if (h == 0 && iteration < FUZZ_ITERS / 10 * 9)
        h = 1 + below(15);
    return h;
}

static uint64_t pick_fd(void)
{
    if (open_fd_count > 0 && below(2))
        return open_fds[below(open_fd_count)];
    return pick_index(MAX_OPEN_FILES);
}

static uint64_t pick_rights(void)
{
    static const uint64_t edges[] = { 0, 1, 2, 3, 4, 5, 7, 0xFFFF, 0x10000 };
    if (below(4) == 0)
        return rand64();
    return edges[below(sizeof(edges) / sizeof(edges[0]))];
}

static uint64_t pick_mmap_va(void)
{
    switch (below(6)) {
    case 0: return HEAP_BASE + below(32) * 4096; /* around the heap */
    case 1: return HEAP_BASE - 4096;             /* extend downwards */
    case 2: return (uint64_t)__image_rw_start;   /* inside the image */
    case 3: return 0x7FF000 - below(4) * 4096;   /* up to the stack */
    case 4: return below(0x800) * 4096;          /* any page */
    default: return rand64();
    }
}

static void track_fd(uint64_t fd)
{
    if (open_fd_count < MAX_OPEN_FILES)
        open_fds[open_fd_count++] = fd;
}

/*
 * Every syscall except SYS_EXIT (ends the campaign) and SYS_GPU_PLEDGE
 * (irreversibly restricts every later call; covered by test_gpu).
 */
static const uint64_t syscalls[] = {
    SYS_SERIAL_WRITE, SYS_CAP_GRANT, SYS_CAP_REVOKE, SYS_CAP_RESTRICT,
    SYS_CAP_INSPECT, SYS_CHANNEL_SEND, SYS_CHANNEL_RECV, SYS_FS_OPEN,
    SYS_FS_READ, SYS_FS_STAT, SYS_FS_CLOSE, SYS_GPU_DMA, SYS_GPU_UNVEIL,
    SYS_MMAP, SYS_YIELD, SYS_PERF_COUNTER,
};

/* Unassigned numbers next to real ones, and values that differ only above bit 31 */
static const uint64_t unknown_syscalls[] = {
    2, 10, 15, 20, 23, 29, 34, 39, 44, 61, 63, 70, 72, 255,
    (1ULL << 32) | SYS_SERIAL_WRITE, (1ULL << 63) | SYS_FS_OPEN, ~0ULL,
};

static uint64_t fuzz_one(void)
{
    uint64_t nr;
    if (below(10) == 0)
        nr = unknown_syscalls[below(sizeof(unknown_syscalls) / sizeof(unknown_syscalls[0]))];
    else
        nr = syscalls[below(sizeof(syscalls) / sizeof(syscalls[0]))];

    uint64_t a0 = rand64(), a1 = rand64(), a2 = rand64();
    switch (nr) {
    case SYS_SERIAL_WRITE:
        a0 = echo_ptr();
        a1 = pick_len();
        break;
    case SYS_CAP_GRANT:
        /* Favour the boot-granted capability so grants get past the source check */
        a0 = below(2) ? 0 : pick_index(16);
        a1 = pick_index(8);
        a2 = pick_rights();
        break;
    case SYS_CAP_REVOKE:
        a0 = pick_destructive_handle();
        break;
    case SYS_CAP_INSPECT:
        a0 = pick_index(16);
        break;
    case SYS_CAP_RESTRICT:
        a0 = pick_destructive_handle();
        a1 = pick_rights();
        break;
    case SYS_CHANNEL_SEND:
        a0 = below(2) ? FUZZ_CHANNEL : pick_index(4);
        a1 = read_ptr();
        a2 = pick_len();
        break;
    case SYS_CHANNEL_RECV:
        /* This shard holds no RECV right, so a receive can never block */
        a0 = below(2) ? FUZZ_CHANNEL : pick_index(4);
        a1 = write_ptr();
        a2 = pick_len();
        break;
    case SYS_FS_OPEN:
        switch (below(3)) {
        case 0: a0 = (uint64_t)"/hello.txt"; a1 = 10; break;
        case 1: a0 = (uint64_t)"/model.bin"; a1 = 10; break;
        default: a0 = read_ptr(); a1 = pick_len(); break;
        }
        break;
    case SYS_FS_READ:
        a0 = pick_fd();
        a1 = write_ptr();
        a2 = pick_len();
        break;
    case SYS_FS_STAT:
    case SYS_FS_CLOSE:
        a0 = pick_fd();
        break;
    case SYS_GPU_DMA:
        a0 = pick_index(2);
        a1 = pick_len();
        a2 = (pick_len() << 32) | (pick_len() & 0xFFFFFFFF);
        break;
    case SYS_GPU_UNVEIL:
        a0 = pick_len();
        a1 = pick_len();
        break;
    case SYS_MMAP:
        a0 = pick_mmap_va();
        a1 = pick_len();
        break;
    }

    uint64_t r = coconut_syscall3(nr, a0, a1, a2);
    if (nr == SYS_FS_OPEN && r != COCONUT_ERROR)
        track_fd(r);
    return r;
}

/* FNV-1a over text and rodata. No syscall may ever write there. */
static uint64_t image_ro_checksum(void)
{
    uint64_t h = 0xCBF29CE484222325ULL;
    for (const volatile char *p = (const char *)0x1000; p < __image_rw_start; p++) {
        h ^= (uint8_t)*p;
        h *= 0x100000001B3ULL;
    }
    return h;
}

static void run_campaign(void)
{
    rng_state = FUZZ_SEED ? FUZZ_SEED : coconut_perf_counter();
    uint64_t seed = rng_state;
    for (int i = 0; i < (int)sizeof(quiet); i++)
        quiet[i] = '.';

    uint64_t before = image_ro_checksum();
    uint64_t accepted = 0;
    for (iteration = 0; iteration < FUZZ_ITERS; iteration++) {
        if (fuzz_one() != COCONUT_ERROR)
            accepted++;
    }
    coconut_puts("\n");

    /* Close by number, not by the tracked list: it can hold stale entries */
    for (uint64_t fd = 0; fd < MAX_OPEN_FILES; fd++)
        coconut_fs_close(fd);

    struct line l = { .len = 0 };
    line_str(&l, "fuzz: campaign seed=");
    line_u64(&l, seed);
    line_str(&l, " iterations=");
    line_u64(&l, FUZZ_ITERS);
    line_str(&l, " accepted=");
    line_u64(&l, accepted);
    line_end(&l);

    check(image_ro_checksum() == before, "campaign: text and rodata unchanged");
    check(free_descriptors() == MAX_OPEN_FILES, "campaign: no descriptors leaked");
}

/* -----------------------------------------------------------------------
 * main
 * ----------------------------------------------------------------------- */

int main(void)
{
    pass_count = 0;
    fail_count = 0;

    coconut_puts("fuzz-syscall: starting syscall boundary tests\n");

    test_serial_write();
    test_filesystem();
    test_mmap();
    test_image_segments();
    test_capabilities();
    test_channels();
    test_perf_counter();
    test_unknown_syscall();
    run_campaign();
    /* gpu tests last — pledge restricts subsequent syscalls */
    test_gpu();

    coconut_puts("\n--- Fuzz Summary ---\n");
    struct line l = { .len = 0 };
    line_str(&l, "fuzz: pass=");
    line_u64(&l, pass_count);
    line_str(&l, " fail=");
    line_u64(&l, fail_count);
    line_str(&l, " total=");
    line_u64(&l, pass_count + fail_count);
    line_end(&l);

    if (fail_count == 0) {
        coconut_puts("fuzz-syscall: ALL TESTS PASSED\n");
    } else {
        coconut_puts("fuzz-syscall: FAILURES DETECTED\n");
    }

    return fail_count;
}
