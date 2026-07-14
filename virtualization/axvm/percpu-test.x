CPU_NUM = 4;
PROVIDE(PAGE_SIZE = 0x1000);
PROVIDE(STACK_SIZE = 0x40000);

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
    .percpu (NOLOAD) : {
        _percpu_start = .;
        _percpu_load_start = .;
        KEEP(*(.percpu.000.header))
        *(SORT_BY_NAME(.percpu.*))
        *(.percpu)
        KEEP(*(.percpu_end))
        _percpu_load_end = .;
        __percpu_start = _percpu_load_start;
        __percpu_end = _percpu_load_end;
        __AX_CPU_AREA_REQUIRED_ALIGNMENT = MAX(64, ALIGNOF(.percpu));
        _percpu_stride = ALIGN(_percpu_load_end - _percpu_load_start,
                               __AX_CPU_AREA_REQUIRED_ALIGNMENT);
        . = _percpu_load_start + _percpu_stride * CPU_NUM;
    }
    _percpu_end = _percpu_start + SIZEOF(.percpu);
    ASSERT(!DEFINED(__AX_CPU_AREA_PREFIX) ||
           __AX_CPU_AREA_PREFIX == _percpu_load_start,
           "CPU area header must be the first per-CPU template object")
    ASSERT(!DEFINED(__AX_CPU_AREA_TEMPLATE_END) ||
           __AX_CPU_AREA_TEMPLATE_END + 1 == _percpu_load_end,
           "CPU area end sentinel must follow every per-CPU template object")
    . = _percpu_end;
}
INSERT AFTER .bss;
