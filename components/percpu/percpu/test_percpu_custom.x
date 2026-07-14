SECTIONS
{
    .ax_percpu_alignment : {
        __AX_PERCPU_ALIGNMENT_START = .;
        KEEP(*(.ax_percpu.align))
        __AX_PERCPU_ALIGNMENT_END = .;
        __AX_PERCPU_LINKER_ALIGNMENT_START = .;
        __AX_PERCPU_LINKER_ALIGNMENT_END =
            . + MAX(64, ALIGNOF(.percpu));
    }

    . = ALIGN(4K);
    .percpu : {
        _percpu_load_start = .;
        KEEP(*(.percpu.000.header))
        *(SORT_BY_NAME(.percpu.*))
        *(.percpu)
        KEEP(*(.percpu_end))
        _percpu_load_end = .;
        __AX_CPU_AREA_REQUIRED_ALIGNMENT = MAX(64, ALIGNOF(.percpu));
    }
    ASSERT(!DEFINED(__AX_CPU_AREA_PREFIX) ||
           __AX_CPU_AREA_PREFIX == _percpu_load_start,
           "CPU area prefix must be at per-CPU template offset zero")
    ASSERT(!DEFINED(__AX_CPU_AREA_TEMPLATE_END) ||
           __AX_CPU_AREA_TEMPLATE_END + 1 == _percpu_load_end,
           "CPU area end sentinel must follow every per-CPU template object")
}
INSERT AFTER .data;
