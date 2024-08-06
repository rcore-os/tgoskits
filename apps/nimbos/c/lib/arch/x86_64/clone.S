// __clone(func, arg, stack)
//         rdi,  rsi,   rdx

// syscall(SYS_clone, stack)
//         rax,         rdi

.global __clone
.hidden __clone
__clone:
    // push arg (%rsi) to stack, set func (%rdi) to %r9
    and $-16, %rdx
    sub $8, %rdx
    mov %rsi, (%rdx)
    mov %rdi, %r9

    // syscall(SYSCALL_CLONE, newsp)
    mov %rdx, %rdi
    mov $56, %rax
    syscall

    test %rax, %rax
    jz  1f
    // parent
    ret
1:
    // child
    xor %rbp, %rbp
    pop %rdi
    call *%r9
    // syscall(SYSCALL_EXIT, ret)
    mov %rax, %rdi
    mov $60, %rax
    syscall
