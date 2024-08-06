.text
.globl _start
_start:
    mov x29, #0
    mov x30, #0
    mov x0, sp
    and sp, x0, #-16
    b   __start_main
