    .sdata : ALIGN(8) {
        *(.sdata)
        . = ALIGN(PAGE_SIZE);
    }

    .tdata : ${tdata_align} {
        _stdata = .;
        *(.tdata .tdata.*)
        _etdata = .;
    } ${tdata_output}
    ${post_tdata_sections}
    ${edata_align}
    _edata = .;
    __kernel_load_end = .;
    ${bss_start_before_tbss}

    .tbss : ${tbss_align} {
        _stbss = .;
        *(.tbss .tbss.*)
        *(.init.bss)
        *(.tcommon)
        _etbss = .;
    } ${tbss_output}

    ${bss_start_after_tbss}
    ${pre_sbss_align}
    .sbss : {
        *(.dynsbss)
        *(.sbss)
        ${sbss_extra}
        *(.scommon)
    } ${bss_output}
    .bss : {
        *(.dynbss)
        *(.bss .bss.*)
        *(COMMON)

        /* CPU stacks */
        ${cpu_stack_align}
        __cpu0_stack = .;
        . += STACK_SIZE;
        __cpu0_stack_top = .;
    } ${bss_output}
    . = ALIGN(PAGE_SIZE);
    __bss_stop = .;
    _end = .;
    __kernel_code_end = .;
