# Shared software GPR stack image; RSP belongs to the hardware frame.
.macro PUSH_GENERAL_REGS include_rax=1
    push    r15
    push    r14
    push    r13
    push    r12
    push    r11
    push    r10
    push    r9
    push    r8
    push    rdi
    push    rsi
    push    rbp
    push    rbx
    push    rdx
    push    rcx
    .if \include_rax
    push    rax
    .else
    sub     rsp, 8
    .endif
.endm

.macro POP_GENERAL_REGS include_rax=1
    .if \include_rax
    pop     rax
    .else
    add     rsp, 8
    .endif
    pop     rcx
    pop     rdx
    pop     rbx
    pop     rbp
    pop     rsi
    pop     rdi
    pop     r8
    pop     r9
    pop     r10
    pop     r11
    pop     r12
    pop     r13
    pop     r14
    pop     r15
.endm
