// Nofault byte copy between kernel addresses. The load and the store are both
// registered in the nofault exception table, whose base-relative record format
// matches the one the user-atomic routines use on this architecture.

.macro _asm_kernel_nofault_extable from, to
    .pushsection __nofault_ex_table, "a"
    .balign 4
    .word \from - _nofault_ex_table_start
    .word \to - _nofault_ex_table_start
    .popsection
.endm

.section .text
.global __axcpu_copy_from_kernel_nofault
.type __axcpu_copy_from_kernel_nofault, @function
__axcpu_copy_from_kernel_nofault:
    // a0 - destination, a1 - source, a2 - length.
    // t0 counts the bytes not copied yet, which is the value a fault reports.
    mv      t0, a2
    beqz    a2, .Lkernel_copy_done

.Lkernel_copy_loop:
1:
    lbu     t1, 0(a1)
2:
    sb      t1, 0(a0)
    addi    a1, a1, 1
    addi    a0, a0, 1
    addi    t0, t0, -1
    bnez    t0, .Lkernel_copy_loop

.Lkernel_copy_done:
    li      a0, 0
    ret

.Lkernel_copy_fault:
    mv      a0, t0
    ret

    _asm_kernel_nofault_extable 1b, .Lkernel_copy_fault
    _asm_kernel_nofault_extable 2b, .Lkernel_copy_fault
.size __axcpu_copy_from_kernel_nofault, . - __axcpu_copy_from_kernel_nofault
