    .data : ALIGN(16) {
        *(.data .data.*)
        *(.init.data)
        *(.sdata .sdata.*)
        ${data_extra}
    }

    .ax_percpu_alignment : {
        __AX_PERCPU_ALIGNMENT_START = .;
        KEEP(*(.ax_percpu.align))
        __AX_PERCPU_ALIGNMENT_END = .;
        __AX_PERCPU_LINKER_ALIGNMENT_START = .;
        __AX_PERCPU_LINKER_ALIGNMENT_END =
            . + MAX(64, ALIGNOF(.percpu));
    }

    .percpu : {
        __percpu_start = .;
        KEEP(*(.percpu.000.header))
        *(SORT_BY_NAME(.percpu.*))
        *(.percpu .percpu_data .percpu_data.*)
        KEEP(*(.percpu_end))
        __percpu_end = .;
        __AX_CPU_AREA_REQUIRED_ALIGNMENT = MAX(64, ALIGNOF(.percpu));
    }
    ASSERT(__AX_CPU_AREA_PREFIX == __percpu_start,
           "CPU area prefix must be at per-CPU template offset zero")
    ASSERT(__AX_CPU_AREA_TEMPLATE_END + 1 == __percpu_end,
           "CPU area end sentinel must follow every per-CPU template object")
    PROVIDE(__PERCPU_TSS_OFFSET = __PERCPU_TSS - __percpu_start);
