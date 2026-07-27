    .sdata : ALIGN(8) {
        *(.sdata)
        . = ALIGN(PAGE_SIZE);
    }

    ${post_tdata_sections}
    /* Preserve the common symbol ABI without creating TLS output sections. */
    _stdata = .;
    _etdata = .;
    ${edata_align}
    _edata = .;
    __kernel_load_end = .;
    ${bss_start_before_tbss}
    _stbss = .;
    _etbss = .;
    ${bss_start_after_tbss}
    ${pre_sbss_align}

    /* Fail the link if a no-TLS final image pulls in any TLS storage. */
    .forbidden_kernel_tls (NOLOAD) : {
        *(.tdata .tdata.* .gnu.linkonce.td.*)
        *(.tbss .tbss.* .gnu.linkonce.tb.*)
        *(.tcommon)
    }
    ASSERT(SIZEOF(.forbidden_kernel_tls) == 0,
           "kernel TLS input requires the someboot/tls feature")

    .sbss : {
        *(.dynsbss)
        *(.sbss)
        ${sbss_extra}
        *(.scommon)
    } ${bss_output}
    .bss : {
        *(.dynbss)
        *(.init.bss)
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
