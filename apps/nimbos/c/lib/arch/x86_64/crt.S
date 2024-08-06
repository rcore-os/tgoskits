.text
.globl _start
_start:
    xor     %rbp, %rbp
    mov     %rsp, %rdi
    andq    $-16, %rsp
    call   __start_main
