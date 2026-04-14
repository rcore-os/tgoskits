OUTPUT_ARCH(%ARCH%)

BASE_ADDRESS = %KERNEL_BASE%;
%LINKER_CONSTANTS%

%ENTRY_DIRECTIVE%
SECTIONS
{
    . = BASE_ADDRESS;
    _skernel = .;

    .text%TEXT_AT% ALIGN(4K) {
        _stext = .;
        KEEP(*(.text.boot))
        *(.text .text.*)
        . = ALIGN(4K);
        _etext = .;
    }

    _srodata = .;
    .rodata%RODATA_AT% ALIGN(4K) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        *(.sdata2 .sdata2.*)
    }

    .init_array%INIT_ARRAY_AT% ALIGN(0x10) {
        __init_array_start = .;
        *(.init_array .init_array.*)
        __init_array_end = .;
    }

    %DWARF%

    . = ALIGN(4K);
    _erodata = .;

    .data%DATA_AT% ALIGN(4K) {
        _sdata = .;
        *(.data.boot_page_table)
        . = ALIGN(4K);
        *(.data .data.*)
        *(.sdata .sdata.*)
        *(.got .got.*)

        . = ALIGN(0x10);
        _sdriver = .;
        KEEP(*(.driver.register*))
        _edriver = .;

        . = ALIGN(0x10);
        _ex_table_start = .;
        KEEP(*(__ex_table))
        _ex_table_end = .;
    }

%LINKME_SECTION%

    .tdata%TDATA_AT% ALIGN(0x10) {
        _stdata = .;
        *(.tdata .tdata.*)
        _etdata = .;
    }

    .tbss%TBSS_AT% ALIGN(0x10) {
        _stbss = .;
        *(.tbss .tbss.*)
        *(.tcommon)
        _etbss = .;
    }

%PERCPU_SECTION%

    . = ALIGN(4K);
    _edata = .;

    .bss%BSS_AT% ALIGN(4K) {
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

SECTIONS {
    scope_local : { KEEP(*(scope_local)) }
}
INSERT AFTER .tbss;
