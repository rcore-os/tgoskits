/* Newer linkers prohibit VMA lower than image base (typically 0x20_0000), even
 * for NOLOAD sections. So we place the ax-percpu section at a high VMA which does
 * not overlap with other sections. This address is chosen arbitrarily, but it's
 * okay as it's never actually used.
 *
 * This is ONLY necessary for test_percpu.x, normal kernels SHOULD place ax-percpu
 * sections in their linker scripts with 0x0 VMA and no NOLOAD attribute as
 * before. See the linker script snippet used in ArceOS as an example:
 *
 *  .ax_percpu_alignment : {
 *      __AX_PERCPU_ALIGNMENT_START = .;
 *      KEEP(*(.ax_percpu.align))
 *      __AX_PERCPU_ALIGNMENT_END = .;
 *  }
 *  . = ALIGN(4K);
 *  .percpu 0x0 : AT(_percpu_start) {
 *      _percpu_load_start = .;
 *      KEEP(*(.percpu.000.header))
 *      *(SORT_BY_NAME(.percpu.*))
 *      *(.percpu)
 *      KEEP(*(.percpu_end))
 *      _percpu_load_end = .;
 *      _percpu_stride = ALIGN(SIZEOF(.percpu), ALIGNOF(.percpu));
 *      . = _percpu_load_start + _percpu_stride * 4;
 *  }
 *  _percpu_start = LOADADDR(.percpu);
 *  _percpu_end = _percpu_start + SIZEOF(.percpu);
 *  . = _percpu_end;
 *
 */
PERCPU_LOAD_BASE = 0x2000000;
CPU_NUM = 4;

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
    _percpu_runtime_cursor = .;
    .percpu ALIGN(PERCPU_LOAD_BASE, MAX(64, ALIGNOF(.percpu))) :
        AT(ALIGN(_percpu_runtime_cursor, MAX(64, ALIGNOF(.percpu)))) {
        _percpu_load_start = .;
        KEEP(*(.percpu.000.header))
        *(SORT_BY_NAME(.percpu.*))
        *(.percpu)
        KEEP(*(.percpu_end))
        _percpu_load_end = .;
        __AX_CPU_AREA_REQUIRED_ALIGNMENT = MAX(64, ALIGNOF(.percpu));
        _percpu_stride = ALIGN(_percpu_load_end - _percpu_load_start,
                               __AX_CPU_AREA_REQUIRED_ALIGNMENT);
        . = _percpu_load_start + _percpu_stride * CPU_NUM;
    }
    _percpu_start = LOADADDR(.percpu);
    _percpu_end = _percpu_start + SIZEOF(.percpu);
    ASSERT(!DEFINED(__AX_CPU_AREA_PREFIX) ||
           __AX_CPU_AREA_PREFIX == _percpu_load_start,
           "CPU area prefix must be at per-CPU template offset zero")
    ASSERT(!DEFINED(__AX_CPU_AREA_TEMPLATE_END) ||
           __AX_CPU_AREA_TEMPLATE_END + 1 == _percpu_load_end,
           "CPU area end sentinel must follow every per-CPU template object")
    . = _percpu_end;
}
INSERT AFTER .bss;
