/* coconutOS supervisor linker script — split VMA/LMA for higher-half kernel */

ENTRY(_start)

KERNEL_PHYS = 0x200000;
/* Top 2 GiB of virtual address space — compatible with code-model=kernel */
KERNEL_VIRT = 0xFFFFFFFF80000000 + KERNEL_PHYS;

SECTIONS
{
    /*
     * Boot trampoline: runs at physical address before higher-half is active.
     * VMA = LMA = physical address. This code sets up page tables and jumps
     * to the higher-half.
     */
    . = KERNEL_PHYS;

    .text.boot ALIGN(4K) : {
        *(.text.boot)
    }

    __boot_end = .;

    /*
     * Everything below has VMA in the higher half but LMA at physical.
     * The trampoline jumps to KERNEL_VIRT after enabling the HHDM.
     */
    . = KERNEL_VIRT + (__boot_end - KERNEL_PHYS);

    .text ALIGN(4K) : AT(KERNEL_PHYS + (ADDR(.text) - KERNEL_VIRT)) {
        *(.text .text.*)
    }

    .rodata ALIGN(4K) : AT(KERNEL_PHYS + (ADDR(.rodata) - KERNEL_VIRT)) {
        *(.rodata .rodata.*)
    }

    .data ALIGN(4K) : AT(KERNEL_PHYS + (ADDR(.data) - KERNEL_VIRT)) {
        *(.data .data.*)
    }

    .bss ALIGN(4K) (NOLOAD) : AT(KERNEL_PHYS + (ADDR(.bss) - KERNEL_VIRT)) {
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

    /* Symbols for the boot trampoline */
    __kernel_phys_start = KERNEL_PHYS;
    __kernel_phys_end = KERNEL_PHYS + (. - KERNEL_VIRT);
    __kernel_virt_start = KERNEL_VIRT;
    __kernel_virt_end = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.note.*)
        *(.comment)
    }
}
