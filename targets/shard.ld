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

/*
 * The supervisor maps the flat image R+X and nothing else, so writable
 * globals would fault on first write. Keep mutable state on the stack
 * or in mmap'd memory.
 */
ASSERT(SIZEOF(.data) == 0 && SIZEOF(.bss) == 0,
       "shard has writable globals (.data/.bss), which are not mapped")
