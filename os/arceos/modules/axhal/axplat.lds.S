OUTPUT_ARCH(%ARCH%)

BASE_ADDRESS = %KERNEL_BASE%;
PHYS_BASE = %KERNEL_BASE_PADDR%;
LOAD_OFFSET = BASE_ADDRESS - PHYS_BASE;
AX_LINKER_LOAD_OFFSET = LOAD_OFFSET;
CPU_NUM = %CPU_NUM%;

ENTRY(_start)
SECTIONS
{
    . = BASE_ADDRESS;
    _skernel = .;

    .text : AT(ADDR(.text) - LOAD_OFFSET) ALIGN(4K) {
        _stext = .;
        KEEP(*(.text.boot))
        *(.text .text.*)
        . = ALIGN(4K);
        _etext = .;
    }

    _srodata = .;
    .rodata : AT(ADDR(.rodata) - LOAD_OFFSET) ALIGN(4K) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        *(.sdata2 .sdata2.*)
    }

    . = ALIGN(4K);
    _erodata = .;

    .data : AT(ADDR(.data) - LOAD_OFFSET) ALIGN(4K) {
        _sdata = .;
        *(.data.boot_page_table)
        . = ALIGN(4K);
        *(.data .data.*)
        *(.sdata .sdata.*)
        *(.got .got.*)
    }

    .tdata : AT(ADDR(.tdata) - LOAD_OFFSET) ALIGN(0x10) {
        _stdata = .;
        *(.tdata .tdata.*)
        _etdata = .;
    }

    .tbss : AT(ADDR(.tbss) - LOAD_OFFSET) ALIGN(0x10) {
        _stbss = .;
        *(.tbss .tbss.*)
        *(.tcommon)
        _etbss = .;
    }

    .ax_percpu_alignment : AT(ADDR(.ax_percpu_alignment) - LOAD_OFFSET) {
        __AX_PERCPU_ALIGNMENT_START = .;
        KEEP(*(.ax_percpu.align))
        __AX_PERCPU_ALIGNMENT_END = .;
        __AX_PERCPU_LINKER_ALIGNMENT_START = .;
        __AX_PERCPU_LINKER_ALIGNMENT_END =
            . + MAX(64, ALIGNOF(.percpu));
    }

    . = ALIGN(4K);
    _percpu_runtime_cursor = .;
    .percpu 0x0 : AT(ALIGN(_percpu_runtime_cursor,
                           MAX(64, ALIGNOF(.percpu))) - LOAD_OFFSET) {
        _percpu_load_start = .;
        KEEP(*(.percpu.000.header))
        *(SORT_BY_NAME(.percpu.*))
        *(.percpu)
        KEEP(*(.percpu_end))
        _percpu_load_end = .;
        __AX_CPU_AREA_REQUIRED_ALIGNMENT = MAX(64, ALIGNOF(.percpu));
        _percpu_stride = ALIGN(_percpu_load_end - _percpu_load_start,
                               __AX_CPU_AREA_REQUIRED_ALIGNMENT);
        . = _percpu_load_start + _percpu_stride * %CPU_NUM%;
    }
    _percpu_start = LOADADDR(.percpu) + LOAD_OFFSET;
    _percpu_end = _percpu_start + SIZEOF(.percpu);
    ASSERT(__AX_CPU_AREA_PREFIX == _percpu_load_start,
           "CPU area prefix must be at per-CPU template offset zero")
    ASSERT(__AX_CPU_AREA_TEMPLATE_END + 1 == _percpu_load_end,
           "CPU area end sentinel must follow every per-CPU template object")
    PROVIDE(__PERCPU_TSS_OFFSET = __PERCPU_TSS - _percpu_load_start);
    . = _percpu_end;

    . = ALIGN(4K);
    _edata = .;

    .bss : AT(ADDR(.bss) - LOAD_OFFSET) ALIGN(4K) {
        boot_stack = .;
        *(.bss.stack)
        . = ALIGN(4K);
        boot_stack_top = .;

        _sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(4K);
        _ebss = .;
    }

    _ekernel = .;

    /DISCARD/ : {
        *(.comment) *(.gnu*) *(.note*) *(.eh_frame*)
    }
}
