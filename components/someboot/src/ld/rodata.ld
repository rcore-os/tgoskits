    . = ALIGN(PAGE_SIZE);
    _etext = .;
    _srodata = .;
    .rodata : {
        *(.rodata)
        *(.rodata.*)
        ${rodata_extra}
        *(.data.rel.ro*)
    }
    . = ALIGN(PAGE_SIZE);
    _erodata = .;
    _sdata = .;
