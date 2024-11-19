.macro SAVE_REGS_FROM_EL1
    # Curretly `sp` points to the address of `Aarch64VCpu.host_stack_top`.
    sub     sp, sp, 34 * 8
    # Curretly `sp` points to the base address of `Aarch64VCpu.ctx`, which stores guest's `TrapFrame`.

    # Save general purpose registers into `Aarch64VCpu.ctx`
    stp     x0, x1, [sp]
    stp     x2, x3, [sp, 2 * 8]
    stp     x4, x5, [sp, 4 * 8]
    stp     x6, x7, [sp, 6 * 8]
    stp     x8, x9, [sp, 8 * 8]
    stp     x10, x11, [sp, 10 * 8]
    stp     x12, x13, [sp, 12 * 8]
    stp     x14, x15, [sp, 14 * 8]
    stp     x16, x17, [sp, 16 * 8]
    stp     x18, x19, [sp, 18 * 8]
    stp     x20, x21, [sp, 20 * 8]
    stp     x22, x23, [sp, 22 * 8]
    stp     x24, x25, [sp, 24 * 8]
    stp     x26, x27, [sp, 26 * 8]
    stp     x28, x29, [sp, 28 * 8]

    mrs     x9, sp_el0
    stp     x30, x9, [sp, 30 * 8]

    # Save `elr_el2` and `spsr_el2` into `Aarch64VCpu.ctx`
    mrs     x10, elr_el2
    mrs     x11, spsr_el2
    stp     x10, x11, [sp, 32 * 8]
.endm

.macro RESTORE_REGS_INTO_EL1
    ldp     x10, x11, [sp, 32 * 8]
    ldp     x30, x9, [sp, 30 * 8]
    msr     sp_el0, x9
    msr     elr_el2, x10
    msr     spsr_el2, x11

    ldr     x30,      [sp, 30 * 8]
    ldp     x28, x29, [sp, 28 * 8]
    ldp     x26, x27, [sp, 26 * 8]
    ldp     x24, x25, [sp, 24 * 8]
    ldp     x22, x23, [sp, 22 * 8]
    ldp     x20, x21, [sp, 20 * 8]
    ldp     x18, x19, [sp, 18 * 8]
    ldp     x16, x17, [sp, 16 * 8]
    ldp     x14, x15, [sp, 14 * 8]
    ldp     x12, x13, [sp, 12 * 8]
    ldp     x10, x11, [sp, 10 * 8]
    ldp     x8, x9, [sp, 8 * 8]
    ldp     x6, x7, [sp, 6 * 8]
    ldp     x4, x5, [sp, 4 * 8]
    ldp     x2, x3, [sp, 2 * 8]
    ldp     x0, x1, [sp]
    # Curretly `sp` points to the base address of `Aarch64VCpu.ctx`
    add     sp, sp, 34 * 8
     # Curretly `x0` points to the address of `Aarch64VCpu.host_stack_top`.
.endm


.macro INVALID_EXCP_EL2, kind, source
.p2align 7
    SAVE_REGS_FROM_EL1
    mov     x0, sp
    mov     x1, \kind
    mov     x2, \source
    bl      invalid_exception_el2
    b       .Lexception_return_el2
.endm

.macro HANDLE_IRQ_VCPU
.p2align 7
    SAVE_REGS_FROM_EL1
    mov    x0, {exception_irq}
    bl     vmexit_trampoline
    # b .Lexception_return_el2 is called by `vmexit_trampoline`
.endm

.macro HANDLE_LOWER_SYNC_VCPU
.p2align 7
    SAVE_REGS_FROM_EL1
    mov    x0, {exception_sync}
    bl     vmexit_trampoline
    # b .Lexception_return_el2 is called by `vmexit_trampoline`
.endm


.section .text
.p2align 11
.global exception_vector_base_vcpu
exception_vector_base_vcpu:
    // current EL, with SP_EL0
    INVALID_EXCP_EL2 0 0
    INVALID_EXCP_EL2 1 0
    INVALID_EXCP_EL2 2 0
    INVALID_EXCP_EL2 3 0

    // current EL, with SP_ELx
    INVALID_EXCP_EL2 0 1
    HANDLE_IRQ_VCPU
    INVALID_EXCP_EL2 2 1
    INVALID_EXCP_EL2 3 1

    // lower EL, aarch64
    HANDLE_LOWER_SYNC_VCPU
    HANDLE_IRQ_VCPU
    INVALID_EXCP_EL2 2 2
    INVALID_EXCP_EL2 3 2

    // lower EL, aarch32
    INVALID_EXCP_EL2 0 3
    INVALID_EXCP_EL2 1 3
    INVALID_EXCP_EL2 2 3
    INVALID_EXCP_EL2 3 3

.global context_vm_entry
context_vm_entry:
    # Curretly `x0` points to the address of `Aarch64VCpu.host_stack_top`.
    mov     sp, x0
    sub     sp, sp, 34 * 8
    # Curretly `sp` points to the base address of `Aarch64VCpu.ctx`, which stores guest's `TrapFrame`.
.Lexception_return_el2:
    RESTORE_REGS_INTO_EL1
    eret
