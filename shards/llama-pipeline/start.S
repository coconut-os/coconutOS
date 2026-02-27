/*
 * Entry stub for the llama-pipeline shard — placed at VA 0x1000 by shard.ld.
 *
 * Sets up the stack pointer, calls main(), and exits with the return value.
 */

.section .text.entry, "ax"
.global _start

_start:
    movq $0x800000, %rsp
    call main

    /* main() returned — exit with its return value */
    movq %rax, %rdi          /* exit code = main()'s return */
    movq $0, %rax            /* SYS_EXIT */
    syscall

    /* unreachable */
1:  hlt
    jmp 1b
