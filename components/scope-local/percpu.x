CPU_NUM = 4;

SECTIONS
{
    .ax_percpu_init : ALIGN(8) {
        __AX_PERCPU_INIT_START = .;
        KEEP(*(.ax_percpu.init))
        __AX_PERCPU_INIT_END = .;
    }

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
        *(SORT_BY_NAME(.percpu.storage*))
        *(SORT_BY_NAME(.percpu.*))
        *(.percpu)
        KEEP(*(.percpu_end))
        _percpu_load_end = .;
        __AX_CPU_AREA_REQUIRED_ALIGNMENT = MAX(64, ALIGNOF(.percpu));
        _percpu_stride = ALIGN(_percpu_load_end - _percpu_load_start,
                               __AX_CPU_AREA_REQUIRED_ALIGNMENT);
        . = _percpu_load_start + _percpu_stride * CPU_NUM;
    }
    _percpu_end = _percpu_start + SIZEOF(.percpu);
    ASSERT(__AX_CPU_AREA_PREFIX == _percpu_load_start,
           "CPU area prefix must be at per-CPU template offset zero")
    ASSERT(__AX_CPU_AREA_TEMPLATE_END + 1 == _percpu_load_end,
           "CPU area end sentinel must follow every per-CPU template object")
    . = _percpu_end;
}
INSERT AFTER .bss;
