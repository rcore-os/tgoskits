.macro SAVE_REGS el
    sub     sp, sp, {trapframe_size}
    AX_CPU_SAVE_GPRS

    .if \el == 2
    mrs     x9, elr_el2
    mrs     x10, spsr_el2
    .else
    mrs     x9, elr_el1
    mrs     x10, spsr_el1
    .endif
    stp     x9, x10, [sp, {elr_offset}]

    add     x9, sp, {trapframe_size}
    str     x9, [sp, {sp_offset}]
.endm

.macro RESTORE_REGS el
    ldp     x9, x10, [sp, {elr_offset}]
    .if \el == 2
    msr     elr_el2, x9
    msr     spsr_el2, x10
    .else
    msr     elr_el1, x9
    msr     spsr_el1, x10
    .endif

    AX_CPU_RESTORE_GPRS
    add     sp, sp, {trapframe_size}
.endm

.macro HANDLE_TRAP, kind, source, el, boot=0
.p2align 7
    SAVE_REGS \el
    mov     x0, sp
    mov     x1, \kind
    mov     x2, \source
    mov     x3, \el
    .if \boot
    bl      __ax_cpu_boot_trap
    .else
    bl      aarch64_trap_handler
    .endif
    b       .Lexception_return\el
.endm

.macro EXIT_USER, kind
.p2align 7
    SAVE_REGS 1
    mov     x0, \kind
    b       .Lexit_user
.endm

.section .text
.macro VECTOR name, el, boot=0
.section .text.\name,"ax"
.p2align 11
.global \name
\name:
    HANDLE_TRAP {TRAP_KIND_SYNC} {TRAP_SRC_CURR_EL0} \el \boot
    HANDLE_TRAP {TRAP_KIND_IRQ} {TRAP_SRC_CURR_EL0} \el \boot
    HANDLE_TRAP {TRAP_KIND_FIQ} {TRAP_SRC_CURR_EL0} \el \boot
    HANDLE_TRAP {TRAP_KIND_SERROR} {TRAP_SRC_CURR_EL0} \el \boot
    HANDLE_TRAP {TRAP_KIND_SYNC} {TRAP_SRC_CURR_ELX} \el \boot
    HANDLE_TRAP {TRAP_KIND_IRQ} {TRAP_SRC_CURR_ELX} \el \boot
    HANDLE_TRAP {TRAP_KIND_FIQ} {TRAP_SRC_CURR_ELX} \el \boot
    HANDLE_TRAP {TRAP_KIND_SERROR} {TRAP_SRC_CURR_ELX} \el \boot
    .if (\el == 1) && (\boot == 0)
    EXIT_USER {TRAP_KIND_SYNC}
    EXIT_USER {TRAP_KIND_IRQ}
    EXIT_USER {TRAP_KIND_FIQ}
    EXIT_USER {TRAP_KIND_SERROR}
    .else
    HANDLE_TRAP {TRAP_KIND_SYNC} {TRAP_SRC_LOWER_AARCH64} \el \boot
    HANDLE_TRAP {TRAP_KIND_IRQ} {TRAP_SRC_LOWER_AARCH64} \el \boot
    HANDLE_TRAP {TRAP_KIND_FIQ} {TRAP_SRC_LOWER_AARCH64} \el \boot
    HANDLE_TRAP {TRAP_KIND_SERROR} {TRAP_SRC_LOWER_AARCH64} \el \boot
    .endif
    HANDLE_TRAP {TRAP_KIND_SYNC} {TRAP_SRC_LOWER_AARCH32} \el \boot
    HANDLE_TRAP {TRAP_KIND_IRQ} {TRAP_SRC_LOWER_AARCH32} \el \boot
    HANDLE_TRAP {TRAP_KIND_FIQ} {TRAP_SRC_LOWER_AARCH32} \el \boot
    HANDLE_TRAP {TRAP_KIND_SERROR} {TRAP_SRC_LOWER_AARCH32} \el \boot
.endm

VECTOR __ax_cpu_vector_el1, 1
VECTOR __ax_cpu_vector_el2, 2
VECTOR __ax_cpu_boot_vector_el1, 1, 1
VECTOR __ax_cpu_boot_vector_el2, 2, 1

.section .text

.p2align 7
.Lexception_return2:
    RESTORE_REGS 2
    eret

.p2align 7
.Lexit_user:
    mov     x1, sp

    mrs     x8, sp_el0
    mrs     x9, tpidr_el0
    ldp     x10, x11, [x1, {trapframe_size}]
    stp     x8, x9, [x1, {trapframe_size}]

    // SP_EL0 is the sole current-context source. `enter_user` spills it in the
    // pinned kernel stack frame before lending SP_EL0 to userspace.
    ldr     x12, [x10, 12 * 8]
    msr     sp_el0, x12

    mov     sp, x10
    msr     tpidr_el0, x11

    ldp     x19, x20, [sp]
    ldp     x21, x22, [sp, 2 * 8]
    ldp     x23, x24, [sp, 4 * 8]
    ldp     x25, x26, [sp, 6 * 8]
    ldp     x27, x28, [sp, 8 * 8]
    ldp     x29, x30, [sp, 10 * 8]
    add     sp, sp, 14 * 8

    ret

.global enter_user
enter_user:
    sub     sp, sp, 14 * 8
    stp     x29, x30, [sp, 10 * 8]
    stp     x27, x28, [sp, 8 * 8]
    stp     x25, x26, [sp, 6 * 8]
    stp     x23, x24, [sp, 4 * 8]
    stp     x21, x22, [sp, 2 * 8]
    stp     x19, x20, [sp]
    mrs     x12, sp_el0
    str     x12, [sp, 12 * 8]

    mov     x10, sp
    mrs     x11, tpidr_el0
    ldp     x8, x9, [x0, {trapframe_size}]
    stp     x10, x11, [x0,{trapframe_size}]
    // Restore user SP/TLS only in the final assembly path to eret.
    msr     sp_el0, x8
    msr     tpidr_el0, x9
    mov     sp, x0

.Lexception_return1:
    RESTORE_REGS 1
    eret
