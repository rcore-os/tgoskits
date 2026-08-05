ENTRY(_start)
PHDRS {
    image PT_LOAD FLAGS(6);
    dynamic PT_DYNAMIC FLAGS(6);
}
SECTIONS {
    . = 0x00400000;
    .text : { KEEP(*(.text.fixture)) } :image
    .data : ALIGN(8) { KEEP(*(.data.fixture)) } :image
    .percpu.template : ALIGN(64) { KEEP(*(.percpu.template.fixture)) } :image
    .dynsym : { *(.dynsym) } :image
    .gnu.hash : { *(.gnu.hash) } :image
    .hash : { *(.hash) } :image
    .dynstr : { *(.dynstr) } :image
    .rela.dyn : ALIGN(8) {
        __rela_dyn_begin = .;
        *(.rela.dyn)
    } :image
    __rela_dyn_end = ADDR(.rela.dyn) + SIZEOF(.rela.dyn);
    .dynamic : { *(.dynamic) } :image :dynamic
    _end = .;
    /DISCARD/ : { *(.comment .note* .eh_frame*) }
}
