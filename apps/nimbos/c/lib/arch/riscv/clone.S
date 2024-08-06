// __clone(func, arg, stack)
//         a0,   a1,    a2

// syscall(SYS_clone, stack)
//         a7,        a0

.global __clone
.hidden __clone
__clone:
    andi    a2, a2, -16
    addi    a2, a2, -16
    sd      a0, 0(a2)
    sd      a1, 8(a2)

    // syscall(SYSCALL_CLONE, newsp)
    mv      a0, a2
    li      a7, 56
    ecall

    beqz    a0, 1f
    // parent
    ret
1:
    // child
    ld      a0, 8(sp)
    ld      a1, 0(sp)
    jalr    a1
    // syscall(SYSCALL_EXIT, ret)
    li      a7, 60
    ecall
