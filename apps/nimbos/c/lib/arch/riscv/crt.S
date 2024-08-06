.text
.globl _start
_start:
.option push
.option norelax
    lla     gp, __global_pointer$
.option pop
    mv      a0, sp
    and     sp, sp, -16
    tail    __start_main
