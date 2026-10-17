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
    ASSERT(!DEFINED(__CPU_LOCAL_AREA_PREFIX) ||
           __CPU_LOCAL_AREA_PREFIX == ADDR(.percpu.template),
           "CPU area prefix must be at template offset zero")
    ASSERT(!DEFINED(__CPU_LOCAL_TEMPLATE_END) ||
           __CPU_LOCAL_TEMPLATE_END + 1 ==
               ADDR(.percpu.template) + SIZEOF(.percpu.template),
           "CPU area end sentinel must follow every template object")
}
INSERT AFTER .data;
