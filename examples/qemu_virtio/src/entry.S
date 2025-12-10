    .section .text.entry
    .globl _start
_start:
    # 设置栈指针
    la sp, boot_stack_top
    call rust_main
    # 不应该返回
1:
    j 1b

    .section .bss.stack
    .globl boot_stack
boot_stack:
    .space 1024*1024*300 #堆内存应该足够大，确保内存的测试文件不会爆栈
    .globl boot_stack_top
boot_stack_top:
