    . = ALIGN(PAGE_SIZE);
    _stext = .;
    .text : {
        *(.text .text.*)
        *(.init.text)
        *(.fixup)
        *(.gnu.warning)
    } ${text_output}
