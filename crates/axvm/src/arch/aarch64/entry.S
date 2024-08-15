.global context_vm_entry
context_vm_entry:
    mov     sp, x0
    sub     sp, sp, 34 * 8

    ldp     x10, x11, [sp, 32 * 8]
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

    # keep sp as host stack top addr
    add     sp, sp, 34 * 8
    eret