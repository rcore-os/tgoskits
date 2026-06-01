INCLUDE "axplat.x"

SECTIONS {
    .init_array : AT(ADDR(.init_array) - AX_LINKER_LOAD_OFFSET) ALIGN(0x10) {
        __init_array_start = .;
        KEEP(*(.init_array .init_array.*))
        __init_array_end = .;
    }

    .runtime : AT(ADDR(.runtime) - AX_LINKER_LOAD_OFFSET) ALIGN(0x10) {
        __sdriver_register = .;
        KEEP(*(.driver.register*))
        __edriver_register = .;

        . = ALIGN(0x10);
        _ex_table_start = .;
        KEEP(*(__ex_table))
        _ex_table_end = .;
    }

    scope_local : AT(ADDR(scope_local) - AX_LINKER_LOAD_OFFSET) {
        KEEP(*(scope_local))
    }
}
INSERT AFTER .rodata;

PROVIDE(_sbss = __bss_start);
PROVIDE(_ebss = __bss_stop);
PROVIDE(boot_stack = __cpu0_stack);
PROVIDE(boot_stack_top = __cpu0_stack_top);
PROVIDE(_percpu_load_start = __percpu_start);
PROVIDE(_percpu_load_end = __percpu_end);
PROVIDE(_percpu_start = __percpu_start);
PROVIDE(_percpu_end = __percpu_end);
PROVIDE(_skernel = VM_LOAD_ADDRESS);
PROVIDE(_ekernel = __kernel_code_end);
