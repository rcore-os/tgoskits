    . = ALIGN(PAGE_SIZE);
    _etext = .;
    _srodata = .;
    .rodata : {
        . = ALIGN(8);
        __someboot_resources_start = .;
        KEEP(*(.someboot.resources))
        __someboot_resources_end = .;
        *(.rodata)
        *(.rodata.*)
        ${rodata_extra}
        *(.data.rel.ro*)
    }
    . = ALIGN(PAGE_SIZE);
    _erodata = .;
    _sdata = .;
