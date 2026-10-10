.global _start
.type _start, %function
_start:
    mov x0, sp
    bl init_main
    mov x8, #93
    svc #0
1:
    b 1b
