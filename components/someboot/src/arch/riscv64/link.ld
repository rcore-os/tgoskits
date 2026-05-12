PROVIDE(PAGE_SIZE = 0x1000);
PROVIDE(STACK_SIZE = 0x4000);
VM_LOAD_ADDRESS = ${kernel_load_vaddr};

OUTPUT_ARCH(riscv)
ENTRY(_head)

SECTIONS
{
    . = VM_LOAD_ADDRESS;
    .head.text : {
        KEEP(*(.head.text))
        KEEP(*(.text._head))
    }
    . = ALIGN(PAGE_SIZE);
    _stext = .;
    .text : {
        *(.text .text.*)
        *(.init.text)
        *(.fixup)
        *(.gnu.warning)
    }
    . = ALIGN(PAGE_SIZE);
    _etext = .;
    _srodata = .;
    .rodata : {
        *(.rodata)
        *(.rodata.*)
        *(.srodata .srodata.*)
        *(.data.rel.ro*)
    }
    . = ALIGN(PAGE_SIZE);
    _erodata = .;
    _sdata = .;
    .data : ALIGN(16) {
        *(.data .data.*)
        *(.init.data)
        *(.sdata .sdata.*)

        . = ALIGN(PAGE_SIZE);
        __percpu_start = .;
        *(.percpu_data .percpu_data.* .percpu .percpu.*)
        __percpu_end = .;
    }
    .rela.dyn : ALIGN(8) {
        __rela_dyn_begin = .;
        *(.rela .rela*)
        __rela_dyn_end = .;
    }
    .sdata : ALIGN(8) {
        *(.sdata)
        . = ALIGN(PAGE_SIZE);
    }
    _edata = .;
    __kernel_load_end = .;
    __bss_start = .;
    .sbss : {
        *(.dynsbss)
        *(.sbss .sbss.*)
        *(.scommon)
    }
    .bss : {
        *(.dynbss)
        *(.bss .bss.*)
        *(COMMON)

        __cpu0_stack = .;
        . += STACK_SIZE;
        __cpu0_stack_top = .;
    }
    . = ALIGN(PAGE_SIZE);
    __bss_stop = .;
    _end = .;
    __kernel_code_end = .;

    /DISCARD/ : {
        *(.eh_frame)
        *(.note .note.*)
        *(.comment)
    }
}
