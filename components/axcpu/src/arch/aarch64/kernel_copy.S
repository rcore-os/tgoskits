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
.type __axcpu_copy_from_kernel_nofault, %function
__axcpu_copy_from_kernel_nofault:
    // x0 - destination, x1 - source, x2 - length.
    // x3 counts the bytes not copied yet, which is the value a fault reports.
    mov     x3, x2
    cbz     x2, .Lkernel_copy_done

.Lkernel_copy_loop:
1:
    ldrb    w4, [x1]
2:
    strb    w4, [x0]
    add     x1, x1, #1
    add     x0, x0, #1
    subs    x3, x3, #1
    b.ne    .Lkernel_copy_loop

.Lkernel_copy_done:
    mov     x0, #0
    ret

.Lkernel_copy_fault:
    mov     x0, x3
    ret

    _asm_kernel_nofault_extable 1b, .Lkernel_copy_fault
    _asm_kernel_nofault_extable 2b, .Lkernel_copy_fault
.size __axcpu_copy_from_kernel_nofault, . - __axcpu_copy_from_kernel_nofault
