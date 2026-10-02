// Nofault byte copy between kernel addresses. The load and the store are both
// registered in the nofault exception table, whose field-relative record
// format matches the one the user-atomic routines use on this architecture.

.macro _asm_kernel_nofault_extable from, to
    .pushsection __nofault_ex_table, "a"
    .balign 4
    .word \from - .
    .word \to - .
    .popsection
.endm

.section .text
.global __axcpu_copy_from_kernel_nofault
.type __axcpu_copy_from_kernel_nofault, @function
__axcpu_copy_from_kernel_nofault:
    // a0 - destination, a1 - source, a2 - length.
    // t0 counts the bytes not copied yet, which is the value a fault reports.
    move    $t0, $a2
    beqz    $a2, .Lkernel_copy_done

.Lkernel_copy_loop:
1:
    ld.b    $t1, $a1, 0
2:
    st.b    $t1, $a0, 0
    addi.d  $a1, $a1, 1
    addi.d  $a0, $a0, 1
    addi.d  $t0, $t0, -1
    bnez    $t0, .Lkernel_copy_loop

.Lkernel_copy_done:
    move    $a0, $zero
    jr      $ra

.Lkernel_copy_fault:
    move    $a0, $t0
    jr      $ra

    _asm_kernel_nofault_extable 1b, .Lkernel_copy_fault
    _asm_kernel_nofault_extable 2b, .Lkernel_copy_fault
.size __axcpu_copy_from_kernel_nofault, . - __axcpu_copy_from_kernel_nofault
