OUTPUT_ARCH(loongarch)
ENTRY(_head)

PHDRS {
    image PT_LOAD FLAGS(7);
}

SECTIONS {
    . = 0;
    .head.text : {
        KEEP(*(.head.text))
    } :image

    . = ALIGN(0x1000);
    _stext = .;
    .text : {
        *(.text .text.*)
        *(.init.text)
    } :image

    . = ALIGN(0x1000);
    _etext = .;
    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        *(.data.rel.ro .data.rel.ro.*)
    } :image

    .data : ALIGN(16) {
        *(.data .data.*)
        *(.sdata .sdata.*)
        *(.got .got.*)
        *(.got.plt .got.plt.*)
        *(.igot .igot.*)
    } :image

    .rela.dyn : ALIGN(8) {
        __rela_dyn_begin = .;
        *(.rela .rela*)
        __rela_dyn_end = .;
    } :image

    .relr.dyn : ALIGN(8) {
        __relr_dyn_begin = .;
        *(.relr.dyn)
        __relr_dyn_end = .;
    } :image

    .la_abs : ALIGN(8) {
        __la_abs_begin = .;
        *(.la_abs)
        __la_abs_end = .;
    } :image

    .pe_data_file_align : {
        BYTE(0);
        . = ALIGN(0x200);
    } :image
    _edata = .;

    __bss_start = .;
    .bss (NOLOAD) : ALIGN(8) {
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
    } :image
    . = ALIGN(0x1000);
    __bss_stop = .;
    _end = .;

    _kernel_rsize = ABSOLUTE(_edata - _etext);
    _kernel_vsize = ABSOLUTE(_end - _etext);

    ASSERT(((_etext - _head) & (0x200 - 1)) == 0,
           "PE text section offset must follow FileAlignment")
    ASSERT((_kernel_rsize & (0x200 - 1)) == 0,
           "PE data section raw size must follow FileAlignment")
    ASSERT((__bss_start & 7) == 0, "BSS start must be 8-byte aligned")
    ASSERT((__bss_stop & 7) == 0, "BSS end must be 8-byte aligned")

    /DISCARD/ : {
        *(.interp)
        *(.dynamic .dynsym .dynstr .hash .gnu.hash)
        *(.gnu.version*)
        *(.plt .plt.*)
        *(.eh_frame .eh_frame_hdr)
        *(.comment .note .note.*)
        *(.debug .debug.* .zdebug .zdebug.*)
        *(.llvm_addrsig)
        *(.gptab.data .gptab.sdata .gptab.bss .gptab.sbss)
    }
}
