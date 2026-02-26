/* coconutOS supervisor linker script — load at 2 MiB physical */

ENTRY(_start)

SECTIONS
{
    . = 0x200000;

    .text ALIGN(4K) : {
        *(.text._start)
        *(.text .text.*)
    }

    .rodata ALIGN(4K) : {
        *(.rodata .rodata.*)
    }

    .data ALIGN(4K) : {
        *(.data .data.*)
    }

    .bss ALIGN(4K) : {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    /* 64 KiB stack, placed after BSS */
    . = ALIGN(16);
    __stack_bottom = .;
    . += 64K;
    __stack_top = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.note.*)
        *(.comment)
    }
}
