OUTPUT_ARCH(%ARCH%)

BASE_ADDRESS = %KERNEL_BASE%;
%EXTRA_LINKER_CONSTANTS%
CPU_NUM = %CPU_NUM%;

%ENTRY_DIRECTIVE%
SECTIONS
{
    . = BASE_ADDRESS;
    _skernel = .;

    .text %TEXT_AT% ALIGN(4K) {
        _stext = .;
        %TEXT_PROLOGUE%
        *(.text .text.*)
        . = ALIGN(4K);
        _etext = .;
    }

    _srodata = .;
    .rodata %RODATA_AT% ALIGN(4K) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        %RODATA_EXTRA_BODY%
    }

    %INIT_ARRAY_SECTION%

    %DWARF%

    . = ALIGN(4K);
    _erodata = .;

    .data %DATA_AT% ALIGN(4K) {
        _sdata = .;
        %DATA_PROLOGUE%
        *(.data .data.*)
        *(.sdata .sdata.*)
        %DATA_EXTRA_BODY%
    }

    %POST_DATA_SECTION%

    .tdata %TDATA_AT% ALIGN(0x10) {
        _stdata = .;
        *(.tdata .tdata.*)
        _etdata = .;
    }

    .tbss %TBSS_AT% ALIGN(0x10) {
        _stbss = .;
        *(.tbss .tbss.*)
        *(.tcommon)
        _etbss = .;
    }

    %PERCPU_SECTION%

    . = ALIGN(4K);
    _edata = .;

    %BSS_PREFIX%
    .bss %BSS_AT% ALIGN(4K) {
        boot_stack = .;
        *(.bss.stack)
        . = ALIGN(4K);
        boot_stack_top = .;

        %BSS_INNER_PREFIX%
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(4K);
        _ebss = .;
    }

    _ekernel = .;

    /DISCARD/ : {
        %DISCARD_BODY%
    }
}

%TRAILING_SECTIONS%
