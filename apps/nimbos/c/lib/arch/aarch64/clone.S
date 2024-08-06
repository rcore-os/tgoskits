// __clone(func, arg, stack)
//         x0,   x1,    x2

// syscall(SYS_clone, stack)
//         x8,        x0

.global __clone
.hidden __clone
__clone:
    and x2, x2, #-16
    stp x0, x1, [x2, #-16]!

    // syscall(SYSCALL_CLONE, newsp)
    mov x0, x2
    mov x8, #56
    svc #0

    cbz x0, 1f
    // parent
    ret
1:
    // child
    ldp x1, x0, [sp], #16
    blr x1
    // syscall(SYSCALL_EXIT, ret)
    mov x8, #60
    svc #0
