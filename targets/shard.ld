/*
 * Shard linker script — ELF executable at VA 0x1000.
 *
 * Three page-aligned, contiguous segments: text (R+X), rodata (R), and
 * data+bss (R+W). The supervisor maps each with exactly these permissions
 * and rejects images that leave gaps between them.
 */

ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5);    /* PF_R | PF_X */
    rodata PT_LOAD FLAGS(4);    /* PF_R */
    data   PT_LOAD FLAGS(6);    /* PF_R | PF_W */
}

SECTIONS
{
    . = 0x1000;

    .text : { *(.text.entry) *(.text .text.*) } :text

    . = ALIGN(4096);
    .rodata : { *(.rodata .rodata.*) } :rodata

    . = ALIGN(4096);
    .data : { *(.data .data.*) } :data
    .bss : { *(.bss .bss.*) *(COMMON) } :data

    /DISCARD/ : { *(.eh_frame) *(.note.*) *(.comment) }
}
