.global context_vm_entry
context_vm_entry:
    // mov sp, x0
    ldp     x10, x11, [x0, 32 * 8]
    msr     elr_el2, x10
    msr     spsr_el2, x11

    ldr     x30,      [x0, 30 * 8]
    ldp     x28, x29, [x0, 28 * 8]
    ldp     x26, x27, [x0, 26 * 8]
    ldp     x24, x25, [x0, 24 * 8]
    ldp     x22, x23, [x0, 22 * 8]
    ldp     x20, x21, [x0, 20 * 8]
    ldp     x18, x19, [x0, 18 * 8]
    ldp     x16, x17, [x0, 16 * 8]
    ldp     x14, x15, [x0, 14 * 8]
    ldp     x12, x13, [x0, 12 * 8]
    ldp     x10, x11, [x0, 10 * 8]
    ldp     x8, x9, [x0, 8 * 8]
    ldp     x6, x7, [x0, 6 * 8]
    ldp     x4, x5, [x0, 4 * 8]
    ldp     x2, x3, [x0, 2 * 8]
    ldp     x0, x1, [x0]
    // add     sp, sp, 34 * 8
    eret