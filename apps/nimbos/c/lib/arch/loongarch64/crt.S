.section .text.entry
.globl _start
_start:
    move $fp, $zero
    move $a0, $sp
    .weak _DYNAMIC
    .hidden _DYNAMIC
    la.local $a1, _DYNAMIC
    bstrins.d $sp, $zero, 3, 0
    b __start_main
