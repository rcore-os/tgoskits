// Nofault byte copy between kernel addresses. The load and the store are both
// registered in the nofault exception table, whose field-relative record
// format matches the one the user-atomic routines use on this architecture.

.macro _asm_kernel_nofault_extable from, to
    .pushsection __nofault_ex_table, "a"
    .balign 4
    .long \from - .
    .long \to - .
    .popsection
.endm

.section .text
.global __axcpu_copy_from_kernel_nofault
.type __axcpu_copy_from_kernel_nofault, @function
__axcpu_copy_from_kernel_nofault:
    // rdi - destination, rsi - source, rdx - length.
    // rcx counts the bytes not copied yet, which is the value a fault reports.
    mov rcx, rdx
    test rdx, rdx
    je .Lkernel_copy_done

.Lkernel_copy_loop:
1:
    movzx eax, byte ptr [rsi]
2:
    mov byte ptr [rdi], al
    inc rsi
    inc rdi
    dec rcx
    jne .Lkernel_copy_loop

.Lkernel_copy_done:
    xor eax, eax
    ret

.Lkernel_copy_fault:
    mov rax, rcx
    ret

    _asm_kernel_nofault_extable 1b, .Lkernel_copy_fault
    _asm_kernel_nofault_extable 2b, .Lkernel_copy_fault
.size __axcpu_copy_from_kernel_nofault, . - __axcpu_copy_from_kernel_nofault
