/*
 * hello-c — proof-of-concept C shard for coconutOS.
 *
 * Demonstrates the C ABI / FFI layer: prints a greeting, reads /hello.txt
 * from the ramdisk via filesystem syscalls, prints its contents, and exits.
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

int main(void)
{
    coconut_puts("Hello from C shard!\n");

    /* Open /hello.txt */
    uint64_t fd = coconut_fs_open("/hello.txt", 10);
    if (fd == COCONUT_ERROR) {
        coconut_puts("C shard: failed to open /hello.txt\n");
        return 1;
    }

    /* Read contents into a stack buffer */
    char buf[256];
    uint64_t bytes = coconut_fs_read(fd, buf, sizeof(buf));
    if (bytes != COCONUT_ERROR && bytes > 0)
        coconut_serial_write(buf, bytes);

    coconut_fs_close(fd);
    coconut_puts("C shard: done\n");

    return 0;
}
