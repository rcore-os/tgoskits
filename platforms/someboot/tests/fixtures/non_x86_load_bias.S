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
        .globl percpu_offset
        .type percpu_offset,@object
percpu_offset:
        .quad percpu_value - percpu_start
        .size percpu_offset, 8

        .section .percpu.fixture,"aw"
        .p2align 6
        .globl percpu_start
        .type percpu_start,@object
percpu_start:
        .zero 64
        .globl percpu_value
        .type percpu_value,@object
percpu_value:
        .quad 0x5a5aa5a513572468
        .size percpu_value, 8
        .globl percpu_end
percpu_end:
