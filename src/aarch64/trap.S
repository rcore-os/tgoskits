.macro SAVE_REGS
    sub     sp, sp, {trapframe_size}
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
    str     x30, [sp, 30 * 8]

    mrs     x9, elr_el1
    mrs     x10, spsr_el1
    stp     x9, x10, [sp, 31 * 8]
.endm

.macro RESTORE_REGS
    ldp     x9, x10, [sp, 31 * 8]
    msr     elr_el1, x9
    msr     spsr_el1, x10

    ldr     x30, [sp, 30 * 8]
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
    add     sp, sp, {trapframe_size}
.endm

.macro HANDLE_TRAP, kind, source
.p2align 7
    SAVE_REGS
    mov     x0, sp
    mov     x1, \kind
    mov     x2, \source
    bl      aarch64_trap_handler
    b       .Lexception_return
.endm

.macro EXIT_USER, kind
.p2align 7
    SAVE_REGS
    mov     x0, \kind
    b       .Lexit_user
.endm

.section .text
.p2align 11
.global exception_vector_base
exception_vector_base:
    // current EL, with SP_EL0
    HANDLE_TRAP {TRAP_KIND_SYNC} {TRAP_SRC_CURR_EL0}
    HANDLE_TRAP {TRAP_KIND_IRQ} {TRAP_SRC_CURR_EL0}
    HANDLE_TRAP {TRAP_KIND_FIQ} {TRAP_SRC_CURR_EL0}
    HANDLE_TRAP {TRAP_KIND_SERROR} {TRAP_SRC_CURR_EL0}

    // current EL, with SP_ELx
    HANDLE_TRAP {TRAP_KIND_SYNC} {TRAP_SRC_CURR_ELX}
    HANDLE_TRAP {TRAP_KIND_IRQ} {TRAP_SRC_CURR_ELX}
    HANDLE_TRAP {TRAP_KIND_FIQ} {TRAP_SRC_CURR_ELX}
    HANDLE_TRAP {TRAP_KIND_SERROR} {TRAP_SRC_CURR_ELX}

    // lower EL, aarch64 {TRAP_SRC_LOWER_AARCH64}
    EXIT_USER {TRAP_KIND_SYNC}
    EXIT_USER {TRAP_KIND_IRQ}
    EXIT_USER {TRAP_KIND_FIQ}
    EXIT_USER {TRAP_KIND_SERROR}

    // lower EL, aarch32
    HANDLE_TRAP {TRAP_KIND_SYNC} {TRAP_SRC_LOWER_AARCH32}
    HANDLE_TRAP {TRAP_KIND_IRQ} {TRAP_SRC_LOWER_AARCH32}
    HANDLE_TRAP {TRAP_KIND_FIQ} {TRAP_SRC_LOWER_AARCH32}
    HANDLE_TRAP {TRAP_KIND_SERROR} {TRAP_SRC_LOWER_AARCH32}

.p2align 7
.Lexit_user:
    mov     x1, sp

    mrs     x8, sp_el0
    mrs     x9, tpidr_el0
    ldp     x10, x11, [x1, {trapframe_size}]
    stp     x8, x9, [x1, {trapframe_size}]
    mov     sp, x10
    msr     tpidr_el0, x11

    ldp     x19, x20, [sp]
    ldp     x21, x22, [sp, 2 * 8]
    ldp     x23, x24, [sp, 4 * 8]
    ldp     x25, x26, [sp, 6 * 8]
    ldp     x27, x28, [sp, 8 * 8]
    ldp     x29, x30, [sp, 10 * 8]
    add     sp, sp, 12 * 8

    ret

.global enter_user
enter_user:
    sub     sp, sp, 12 * 8
    stp     x29, x30, [sp, 10 * 8]
    stp     x27, x28, [sp, 8 * 8]
    stp     x25, x26, [sp, 6 * 8]
    stp     x23, x24, [sp, 4 * 8]
    stp     x21, x22, [sp, 2 * 8]
    stp     x19, x20, [sp]

    mov     x10, sp
    mrs     x11, tpidr_el0
    ldp     x8, x9, [x0, {trapframe_size}]
    stp     x10, x11, [x0,{trapframe_size}]
    msr     sp_el0, x8
    msr     tpidr_el0, x9

    mov     sp, x0

.Lexception_return:
    RESTORE_REGS
    eret
