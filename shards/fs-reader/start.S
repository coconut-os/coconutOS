/*
 * fs-reader shard — placed at VA 0x1000 by shard.ld.
 *
 * Opens /hello.txt, reads it into the stack page, prints it via serial,
 * closes the file, and exits.
 */

.section .text.entry, "ax"
.global _start

_start:
    movq $0x800000, %rsp

    /* SYS_FS_OPEN(path, 10) -> fd */
    leaq path(%rip), %rdi
    movq $10, %rsi           /* path_len = 10 ("/hello.txt") */
    movq $30, %rax           /* SYS_FS_OPEN */
    syscall
    movq %rax, %r12          /* save fd (r12 preserved across syscalls) */

    /* SYS_FS_READ(fd, 0x7FFF00, 256) -> bytes_read */
    movq %r12, %rdi
    movq $0x7FFF00, %rsi     /* buffer in stack page */
    movq $256, %rdx          /* max_len */
    movq $31, %rax           /* SYS_FS_READ */
    syscall
    movq %rax, %r13          /* save bytes_read */

    /* SYS_SERIAL_WRITE(0x7FFF00, bytes_read) -> prints file contents */
    movq $0x7FFF00, %rdi
    movq %r13, %rsi
    movq $1, %rax            /* SYS_SERIAL_WRITE */
    syscall

    /* SYS_FS_CLOSE(fd) */
    movq %r12, %rdi
    movq $33, %rax           /* SYS_FS_CLOSE */
    syscall

    /* SYS_EXIT(0) */
    xorl %edi, %edi
    movq $0, %rax            /* SYS_EXIT */
    syscall

    /* unreachable */
1:  hlt
    jmp 1b

.section .rodata
path:
    .ascii "/hello.txt"
