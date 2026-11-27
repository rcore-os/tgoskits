OUTPUT_ARCH(riscv)
ENTRY(axloader_efi_entry)

PHDRS
{
    image PT_LOAD FLAGS(7);
    dynamic PT_DYNAMIC FLAGS(6);
}

SECTIONS
{
    . = 0;
    __image_start = .;
    .head :
    {
        SHORT(0x5a4d)
        . = 0x3c;
        LONG(0x40)
        LONG(0x4550)
        SHORT(0x5064)                 /* IMAGE_FILE_MACHINE_RISCV64 */
        SHORT(2)
        LONG(0) LONG(0) LONG(0)
        SHORT(160)                   /* PE32+ optional header and six directories */
        SHORT(0x226)                 /* executable; symbols/debug information stripped */
        SHORT(0x20b)
        BYTE(0) BYTE(0)
        LONG(__text_end - __text_start)
        LONG(__data_file_end - __data_start)
        LONG(__image_end - __data_file_end)
        LONG(axloader_efi_entry)
        LONG(__text_start)
        QUAD(0)                      /* self-relocation uses the runtime image base */
        LONG(4096) LONG(512)
        SHORT(0) SHORT(0) SHORT(0) SHORT(0) SHORT(0) SHORT(0)
        LONG(0)
        LONG(__image_end)
        LONG(4096)
        LONG(0)
        SHORT(10) SHORT(0)           /* EFI application */
        QUAD(0) QUAD(0) QUAD(0) QUAD(0)
        LONG(0) LONG(6)
        QUAD(0) QUAD(0) QUAD(0) QUAD(0) QUAD(0) QUAD(0)
        BYTE(0x2e) BYTE(0x74) BYTE(0x65) BYTE(0x78) BYTE(0x74) BYTE(0) BYTE(0) BYTE(0)
        LONG(__text_end - __text_start)
        LONG(__text_start)
        LONG(__text_end - __text_start)
        LONG(__text_start)
        LONG(0) LONG(0) SHORT(0) SHORT(0)
        LONG(0x60000020)
        BYTE(0x2e) BYTE(0x64) BYTE(0x61) BYTE(0x74) BYTE(0x61) BYTE(0) BYTE(0) BYTE(0)
        LONG(__image_end - __data_start)
        LONG(__data_start)
        LONG(__data_file_end - __data_start)
        LONG(__data_start)
        LONG(0) LONG(0) SHORT(0) SHORT(0)
        LONG(0xc0000040)
        . = 4096;
    } :image

    __text_start = .;
    .text : { KEEP(*(.text.efi_entry)) *(.text .text.*) } :image
    .rodata : { *(.rodata .rodata.*) *(.srodata .srodata.*) } :image
    .dynsym : { *(.dynsym) } :image
    .dynstr : { *(.dynstr) } :image
    .gnu.hash : { *(.gnu.hash) } :image
    .hash : { *(.hash) } :image
    .rela.dyn : ALIGN(8)
    {
        __rela_start = .;
        *(.rela.dyn)
        __rela_end = .;
    } :image
    .text_padding : { BYTE(0) . = ALIGN(4096); } :image
    __text_end = .;

    __data_start = .;
    __global_pointer$ = . + 0x800;
    .data : { *(.data .data.*) *(.sdata .sdata.*) *(.got .got.*) } :image
    .dynamic : { *(.dynamic) } :image :dynamic
    .data_padding : { BYTE(0) . = ALIGN(512); } :image
    __data_file_end = .;
    .bss (NOLOAD) : { *(.bss .bss.*) *(.sbss .sbss.*) *(COMMON) . = ALIGN(4096); } :image
    __image_end = .;

    /DISCARD/ : { *(.eh_frame .eh_frame_hdr) *(.comment) *(.note .note.*) }
}

ASSERT(__image_end < 0x100000000, "EFI image RVAs must fit in 32 bits")
ASSERT((__rela_end - __rela_start) % 24 == 0, "invalid ELF64 relocation table")
