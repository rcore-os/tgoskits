        .section .text.fixture,"ax"
        .globl _start
        .type _start,@function
_start:
        .quad 0
        .size _start, . - _start

        .section .data.fixture,"aw"
        .p2align 3
        .globl target_ptr
        .type target_ptr,@object
target_ptr:
        .quad marker
        .size target_ptr, 8

        .globl marker
        .hidden marker
        .type marker,@object
marker:
        .quad 0x246813575a5aa5a5
        .size marker, 8

        /* This same-section difference must remain a scalar, not a relocation. */
        .globl cpu_local_template_offset
        .type cpu_local_template_offset,@object
cpu_local_template_offset:
        .quad cpu_local_template_value - __CPU_LOCAL_AREA_PREFIX
        .size cpu_local_template_offset, 8

        .section .percpu.template.fixture,"aw"
        .p2align 6
        .globl __CPU_LOCAL_AREA_PREFIX
        .type __CPU_LOCAL_AREA_PREFIX,@object
__CPU_LOCAL_AREA_PREFIX:
        .zero 64
        .globl cpu_local_template_value
        .type cpu_local_template_value,@object
cpu_local_template_value:
        .quad 0x5a5aa5a513572468
        .size cpu_local_template_value, 8
        .globl __CPU_LOCAL_TEMPLATE_END
__CPU_LOCAL_TEMPLATE_END:
