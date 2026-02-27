/* Shard linker script — flat binary at VA 0x1000 */

ENTRY(_start)

SECTIONS
{
    . = 0x1000;

    .text : { *(.text.entry) *(.text .text.*) }
    .rodata : { *(.rodata .rodata.*) }
    .data : { *(.data .data.*) }
    .bss (NOLOAD) : { *(.bss .bss.*) }

    /DISCARD/ : { *(.eh_frame) *(.note.*) *(.comment) }
}
