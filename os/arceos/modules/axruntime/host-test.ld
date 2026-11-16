PROVIDE(__CPU_LOCAL_TSS_OFFSET = 0);

SECTIONS
{
    .percpu.init : ALIGN(8) {
        __PERCPU_INIT_START = .;
        KEEP(*(.percpu.init))
        __PERCPU_INIT_END = .;
    }

    .percpu.align : ALIGN(8) {
        __PERCPU_ALIGN_START = .;
        KEEP(*(.percpu.align))
        __PERCPU_ALIGN_END = .;
        __PERCPU_TEMPLATE_ALIGN_START = .;
        __PERCPU_TEMPLATE_ALIGN_END =
            . + MAX(64, ALIGNOF(.percpu.template));
    }

    .percpu.template : {
        KEEP(*(.percpu.template.header))
        *(SORT_BY_NAME(.percpu.template.storage*))
        KEEP(*(.percpu.template.end))
    }

    .host_test_tls : {
        _stdata = .;
        _etdata = .;
        _stbss = .;
        _etbss = .;
    }

    __ex_table : ALIGN(4) {
        PROVIDE(_ex_table_start = .);
        *(__ex_table)
        PROVIDE(_ex_table_end = .);
    }

    __nofault_ex_table : ALIGN(4) {
        PROVIDE(_nofault_ex_table_start = .);
        *(__nofault_ex_table)
        PROVIDE(_nofault_ex_table_end = .);
    }
    ASSERT(__CPU_LOCAL_AREA_PREFIX == ADDR(.percpu.template),
           "CPU area prefix must be at template offset zero")
    ASSERT(__CPU_LOCAL_TEMPLATE_END + 1 ==
               ADDR(.percpu.template) + SIZEOF(.percpu.template),
           "CPU area end sentinel must follow every template object")
}
INSERT AFTER .data;
