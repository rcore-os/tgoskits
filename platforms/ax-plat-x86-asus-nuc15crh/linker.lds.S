OUTPUT_ARCH(%ARCH%)

BASE_ADDRESS = %KERNEL_BASE%;
PHYS_BASE = %KERNEL_BASE_PADDR%;
LOAD_OFFSET = BASE_ADDRESS - PHYS_BASE;
AX_LINKER_LOAD_OFFSET = LOAD_OFFSET;
SMP = %SMP%;

ENTRY(_start)
PHDRS
{
    kernel PT_LOAD FLAGS(7);
    percpu PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = BASE_ADDRESS;
    _skernel = .;

    .text : AT(ADDR(.text) - LOAD_OFFSET) ALIGN(4K) {
        _stext = .;
        *(.text.boot)
        *(.text .text.*)
        . = ALIGN(4K);
        _etext = .;
    } :kernel

    .rodata : AT(ADDR(.rodata) - LOAD_OFFSET) ALIGN(4K) {
        _srodata = .;
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        *(.sdata2 .sdata2.*)
        . = ALIGN(4K);
        _erodata = .;
    } :kernel

    .data : AT(ADDR(.data) - LOAD_OFFSET) ALIGN(4K) {
        _sdata = .;
        *(.data.boot_page_table)
        . = ALIGN(4K);
        *(.data .data.*)
        *(.sdata .sdata.*)
        *(.got .got.*)
    } :kernel

    .tdata : AT(ADDR(.tdata) - LOAD_OFFSET) ALIGN(0x10) {
        _stdata = .;
        *(.tdata .tdata.*)
        _etdata = .;
    } :kernel

    .tbss : AT(ADDR(.tbss) - LOAD_OFFSET) ALIGN(0x10) {
        _stbss = .;
        *(.tbss .tbss.*)
        *(.tcommon)
        _etbss = .;
    } :kernel

    . = ALIGN(4K);
    _percpu_start = .;
    _percpu_end = _percpu_start + SIZEOF(.percpu);
    .percpu 0x0 : AT(_percpu_start - LOAD_OFFSET) {
        _percpu_load_start = .;
        *(.percpu .percpu.*)
        _percpu_load_end = .;
        . = _percpu_load_start + ALIGN(64) * SMP;
    } :percpu
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
    } :kernel

    _ekernel = .;

	/DISCARD/ : {
        *(.comment) *(.gnu*) *(.note*) *(.eh_frame*)
    }
}
