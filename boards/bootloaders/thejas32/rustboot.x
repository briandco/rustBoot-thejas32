/* rustBoot for THEJAS32: linker script.

   The boot ROM copies the start of SPI flash into IRAM at 0x200000 and jumps
   there, which is also where applications are linked. So rustBoot is linked
   to run from the top 96 KB of IRAM. The first thing it runs, `_reloc_start`,
   is position-independent: it copies the image from wherever the ROM put it
   up to this region and jumps into it, which leaves 0x200000..0x228000
   (160 KB) for the application, and for receiving updates.

   IRAM is 0x200000..0x240000 (256 KB, README "Memory Map", measured). The ROM
   uses the top 6 KB as its stack only while loading, before rustBoot runs. */

MEMORY
{
    RB (rwx) : ORIGIN = 0x228000, LENGTH = 96K
}

ENTRY(_reloc_start)

SECTIONS
{
    .text : ALIGN(4)
    {
        __image_start = .;
        KEEP(*(.text.reloc))       /* must be first: it runs at the load address */
        *(.text .text.*)
    } > RB

    .rodata : ALIGN(4)
    {
        *(.srodata .srodata.*)
        *(.rodata .rodata.*)
    } > RB

    .data : ALIGN(4)
    {
        *(.sdata .sdata.*)
        *(.data .data.*)
    } > RB

    /* Last loaded bytes. rustBoot checks this marker after relocating, to
       detect a ROM that copied only part of the image. */
    .image_tail : ALIGN(4)
    {
        KEEP(*(.image_tail))
        . = ALIGN(4);
        __image_end = .;
    } > RB

    .bss (NOLOAD) : ALIGN(4)
    {
        __sbss = .;
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4);
        __ebss = .;
    } > RB

    __stack_top = ORIGIN(RB) + LENGTH(RB);

    /DISCARD/ : { *(.eh_frame) *(.eh_frame_hdr) }
}

/* Must match rb-core/src/layout.rs (APP_REGION_SIZE, RUSTBOOT_SLOT_END): the application may occupy 0x200000 up to here. */
ASSERT(ORIGIN(RB) >= 0x200000 + 0x28000, "rustBoot overlaps the application load region")
ASSERT(__ebss + 8K <= __stack_top, "rustBoot: less than 8 KB left for the stack")
ASSERT(__image_end - __image_start <= 0x20000, "rustBoot image exceeds its 128 KB flash slot")
