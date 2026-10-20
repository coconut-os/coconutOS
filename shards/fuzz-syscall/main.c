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

/* -----------------------------------------------------------------------
 * Test infrastructure
 * ----------------------------------------------------------------------- */

/*
 * Shards are loaded as flat R+X images with no writable data segment, so
 * test state lives on the stack and is threaded through every test.
 */
struct tally {
    int pass;
    int fail;
};

static void print_u64(uint64_t v)
{
    char buf[20];
    int len = 0;
    if (v == 0) {
        buf[0] = '0';
        len = 1;
    } else {
        while (v > 0) {
            buf[len++] = '0' + (v % 10);
            v /= 10;
        }
        for (int i = 0; i < len / 2; i++) {
            char tmp = buf[i];
            buf[i] = buf[len - 1 - i];
            buf[len - 1 - i] = tmp;
        }
    }
    coconut_serial_write(buf, len);
}

static void check(struct tally *t, int condition, const char *name)
{
    if (condition) {
        coconut_puts("PASS: ");
        t->pass++;
    } else {
        coconut_puts("FAIL: ");
        t->fail++;
    }
    coconut_puts(name);
    coconut_puts("\n");
}

/* -----------------------------------------------------------------------
 * SYS_SERIAL_WRITE tests
 * ----------------------------------------------------------------------- */

static void test_serial_write(struct tally *t)
{
    /* Valid write from code region */
    uint64_t r = coconut_serial_write("ok", 2);
    coconut_puts("\n");
    check(t, r == 0, "serial_write: valid buf in code");

    /* Zero length */
    r = coconut_serial_write("x", 0);
    check(t, r == COCONUT_ERROR, "serial_write: len=0 rejected");

    /* Length exceeds limit (4096) */
    r = coconut_syscall2(SYS_SERIAL_WRITE, 0x1000, 4097);
    check(t, r == COCONUT_ERROR, "serial_write: len=4097 rejected");

    /* Null pointer */
    r = coconut_syscall2(SYS_SERIAL_WRITE, 0, 1);
    check(t, r == COCONUT_ERROR, "serial_write: ptr=0 rejected");

    /* Pointer in kernel space */
    r = coconut_syscall2(SYS_SERIAL_WRITE, 0xFFFFFFFF80000000ULL, 1);
    check(t, r == COCONUT_ERROR, "serial_write: kernel ptr rejected");

    /* Pointer wrap-around */
    r = coconut_syscall2(SYS_SERIAL_WRITE, 0xFFFFFFFFFFFFFFFFULL, 2);
    check(t, r == COCONUT_ERROR, "serial_write: ptr overflow rejected");

    /* Pointer just past stack top */
    r = coconut_syscall2(SYS_SERIAL_WRITE, 0x800000, 1);
    check(t, r == COCONUT_ERROR, "serial_write: past stack top rejected");

    /* Valid write from stack region */
    char stack_buf[4] = "stk";
    r = coconut_serial_write(stack_buf, 3);
    coconut_puts("\n");
    check(t, r == 0, "serial_write: valid buf on stack");
}

/* -----------------------------------------------------------------------
 * SYS_FS_* tests
 * ----------------------------------------------------------------------- */

static void test_filesystem(struct tally *t)
{
    /* Open valid file */
    uint64_t fd = coconut_fs_open("/hello.txt", 10);
    check(t, fd != COCONUT_ERROR, "fs_open: valid file");

    /* Stat */
    uint64_t size = coconut_fs_stat(fd);
    check(t, size != COCONUT_ERROR && size > 0, "fs_stat: returns size");

    /* Read */
    char buf[64];
    uint64_t r = coconut_fs_read(fd, buf, 64);
    check(t, r != COCONUT_ERROR && r > 0, "fs_read: returns bytes");

    /* Close */
    r = coconut_fs_close(fd);
    check(t, r == 0, "fs_close: success");

    /* Read after close */
    r = coconut_fs_read(fd, buf, 64);
    check(t, r == COCONUT_ERROR, "fs_read: closed fd rejected");

    /* Open nonexistent */
    r = coconut_fs_open("/nope.txt", 8);
    check(t, r == COCONUT_ERROR, "fs_open: nonexistent rejected");

    /* Open with zero length path */
    r = coconut_syscall2(SYS_FS_OPEN, 0x1000, 0);
    check(t, r == COCONUT_ERROR, "fs_open: path_len=0 rejected");

    /* Open with invalid pointer */
    r = coconut_syscall2(SYS_FS_OPEN, 0, 5);
    check(t, r == COCONUT_ERROR, "fs_open: null path rejected");

    /* Stat on invalid fd */
    r = coconut_fs_stat(999);
    check(t, r == COCONUT_ERROR, "fs_stat: bad fd rejected");

    /* Close on invalid fd */
    r = coconut_fs_close(999);
    check(t, r == COCONUT_ERROR, "fs_close: bad fd rejected");

    /* Read with invalid buf ptr */
    fd = coconut_fs_open("/hello.txt", 10);
    r = coconut_syscall3(SYS_FS_READ, fd, 0, 10);
    check(t, r == COCONUT_ERROR, "fs_read: null buf rejected");
    coconut_fs_close(fd);

    /* Read with zero length */
    fd = coconut_fs_open("/hello.txt", 10);
    r = coconut_syscall3(SYS_FS_READ, fd, (uint64_t)buf, 0);
    check(t, r == COCONUT_ERROR, "fs_read: len=0 rejected");
    coconut_fs_close(fd);
}

/* -----------------------------------------------------------------------
 * SYS_MMAP tests
 * ----------------------------------------------------------------------- */

static void test_mmap(struct tally *t)
{
    /* Valid mmap — allocate heap */
    uint64_t r = coconut_mmap(0x100000, 4);
    check(t, r == 0, "mmap: valid allocation");

    /* Write and read back to verify pages are mapped */
    volatile uint32_t *p = (volatile uint32_t *)0x100000;
    *p = 0xDEADBEEF;
    check(t, *p == 0xDEADBEEF, "mmap: read-write works");

    /* Not page-aligned */
    r = coconut_mmap(0x100001, 1);
    check(t, r == COCONUT_ERROR, "mmap: unaligned rejected");

    /* Zero pages */
    r = coconut_mmap(0x200000, 0);
    check(t, r == COCONUT_ERROR, "mmap: zero pages rejected");

    /* Too many pages */
    r = coconut_mmap(0x200000, 257);
    check(t, r == COCONUT_ERROR, "mmap: 257 pages rejected");

    /* Overlaps code region */
    r = coconut_mmap(0x1000, 1);
    check(t, r == COCONUT_ERROR, "mmap: overlap code rejected");

    /* Overlaps stack */
    r = coconut_mmap(0x7FF000, 1);
    check(t, r == COCONUT_ERROR, "mmap: overlap stack rejected");

    /* Non-contiguous (gap between existing data region) */
    r = coconut_mmap(0x200000, 1);
    check(t, r == COCONUT_ERROR, "mmap: non-contiguous rejected");

    /* Contiguous extension (immediately after existing) */
    r = coconut_mmap(0x104000, 1);
    check(t, r == 0, "mmap: contiguous extension ok");

    /* Overflow: va_start + size wraps */
    r = coconut_mmap(0xFFFFFFFFF000ULL, 2);
    check(t, r == COCONUT_ERROR, "mmap: overflow rejected");
}

/* -----------------------------------------------------------------------
 * SYS_CAP_* tests
 * ----------------------------------------------------------------------- */

static void test_capabilities(struct tally *t)
{
    /* Inspect on empty slot */
    uint64_t r = coconut_syscall1(SYS_CAP_INSPECT, 0);
    check(t, r == COCONUT_ERROR, "cap_inspect: empty slot returns error");

    /* Inspect on out-of-bounds handle */
    r = coconut_syscall1(SYS_CAP_INSPECT, 99);
    check(t, r == COCONUT_ERROR, "cap_inspect: handle=99 rejected");

    /* Revoke on empty slot */
    r = coconut_syscall1(SYS_CAP_REVOKE, 0);
    check(t, r == COCONUT_ERROR, "cap_revoke: empty slot returns error");

    /* Restrict on empty slot */
    r = coconut_syscall2(SYS_CAP_RESTRICT, 0, 0);
    check(t, r == COCONUT_ERROR, "cap_restrict: empty slot returns error");

    /* Grant to invalid shard */
    r = coconut_syscall3(SYS_CAP_GRANT, 0, 255, 0xFFFF);
    check(t, r == COCONUT_ERROR, "cap_grant: target_shard=255 rejected");

    /* Grant from empty handle */
    r = coconut_syscall3(SYS_CAP_GRANT, 0, 0, 0xFFFF);
    check(t, r == COCONUT_ERROR, "cap_grant: empty handle rejected");
}

/* -----------------------------------------------------------------------
 * SYS_CHANNEL_* tests
 * ----------------------------------------------------------------------- */

static void test_channels(struct tally *t)
{
    char buf[16] = "test";

    /* Send on channel we don't own */
    uint64_t r = coconut_channel_send(0, buf, 4);
    check(t, r == COCONUT_ERROR, "channel_send: no cap rejected");

    /* Send on out-of-bounds channel */
    r = coconut_syscall3(SYS_CHANNEL_SEND, 99, (uint64_t)buf, 4);
    check(t, r == COCONUT_ERROR, "channel_send: channel=99 rejected");

    /* Send with zero length */
    r = coconut_syscall3(SYS_CHANNEL_SEND, 0, (uint64_t)buf, 0);
    check(t, r == COCONUT_ERROR, "channel_send: len=0 rejected");

    /* Send with length > 256 */
    r = coconut_syscall3(SYS_CHANNEL_SEND, 0, (uint64_t)buf, 257);
    check(t, r == COCONUT_ERROR, "channel_send: len=257 rejected");

    /* Send with invalid buffer */
    r = coconut_syscall3(SYS_CHANNEL_SEND, 0, 0, 4);
    check(t, r == COCONUT_ERROR, "channel_send: null buf rejected");

    /* Recv on channel we don't own */
    r = coconut_channel_recv(0, buf, 16);
    check(t, r == COCONUT_ERROR, "channel_recv: no cap rejected");
}

/* -----------------------------------------------------------------------
 * SYS_GPU_* tests
 * ----------------------------------------------------------------------- */

static void test_gpu(struct tally *t)
{
    /* DMA without owning a partition */
    uint64_t r = coconut_gpu_dma(0, 0, (0ULL << 32) | 64);
    check(t, r == COCONUT_ERROR, "gpu_dma: no partition rejected");

    /* DMA to invalid partition */
    r = coconut_gpu_dma(99, 0, (0ULL << 32) | 64);
    check(t, r == COCONUT_ERROR, "gpu_dma: partition=99 rejected");

    /* Pledge — should succeed (monotonic AND with unrestricted) */
    r = coconut_gpu_pledge(PLEDGE_SERIAL | PLEDGE_PERF);
    check(t, r == 0, "gpu_pledge: restrict to serial+perf");

    /* After pledge, channel send should be denied */
    char buf[4] = "x";
    r = coconut_syscall3(SYS_CHANNEL_SEND, 0, (uint64_t)buf, 1);
    check(t, r == COCONUT_ERROR, "pledge: channel_send denied after pledge");

    /* Serial should still work */
    r = coconut_serial_write(".", 1);
    check(t, r == 0, "pledge: serial_write still allowed");

    /* Perf counter should still work */
    r = coconut_perf_counter();
    check(t, r != COCONUT_ERROR, "pledge: perf_counter still allowed");

    /* Unveil without a partition */
    r = coconut_gpu_unveil(0, 4096);
    check(t, r == COCONUT_ERROR, "gpu_unveil: no partition rejected");
}

/* -----------------------------------------------------------------------
 * SYS_PERF_COUNTER tests
 * ----------------------------------------------------------------------- */

static void test_perf_counter(struct tally *t)
{
    uint64_t t0 = coconut_perf_counter();
    check(t, t0 != COCONUT_ERROR, "perf_counter: returns value");

    uint64_t t1 = coconut_perf_counter();
    check(t, t1 >= t0, "perf_counter: monotonic");
}

/* -----------------------------------------------------------------------
 * Unknown syscall test
 * ----------------------------------------------------------------------- */

static void test_unknown_syscall(struct tally *t)
{
    /* Syscall number that doesn't exist */
    uint64_t r = coconut_syscall0(9999);
    check(t, r == COCONUT_ERROR, "unknown syscall 9999 rejected");

    r = coconut_syscall0(63);
    check(t, r == COCONUT_ERROR, "unknown syscall 63 rejected");
}

/* -----------------------------------------------------------------------
 * main
 * ----------------------------------------------------------------------- */

int main(void)
{
    struct tally t = { 0, 0 };

    coconut_puts("fuzz-syscall: starting syscall boundary tests\n");

    test_serial_write(&t);
    test_filesystem(&t);
    test_mmap(&t);
    test_capabilities(&t);
    test_channels(&t);
    test_perf_counter(&t);
    test_unknown_syscall(&t);
    /* gpu tests last — pledge restricts subsequent syscalls */
    test_gpu(&t);

    coconut_puts("\n--- Fuzz Summary ---\n");
    coconut_puts("fuzz: pass=");
    print_u64(t.pass);
    coconut_puts(" fail=");
    print_u64(t.fail);
    coconut_puts(" total=");
    print_u64(t.pass + t.fail);
    coconut_puts("\n");

    if (t.fail == 0) {
        coconut_puts("fuzz-syscall: ALL TESTS PASSED\n");
    } else {
        coconut_puts("fuzz-syscall: FAILURES DETECTED\n");
    }

    return t.fail;
}
