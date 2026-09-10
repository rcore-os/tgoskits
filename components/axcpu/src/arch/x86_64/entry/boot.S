# Early CPL0 exceptions: native GPR layout, no runtime TLS or CPU anchor.
.section .text.ax_cpu_boot_vectors,"ax"
.p2align 4
.global __ax_cpu_x86_boot_0
__ax_cpu_x86_boot_0:
    push 0
    push 0
    jmp .Lboot_common
.global __ax_cpu_x86_boot_1
__ax_cpu_x86_boot_1:
    push 0
    push 1
    jmp .Lboot_common
.global __ax_cpu_x86_boot_2
__ax_cpu_x86_boot_2:
    push 0
    push 2
    jmp .Lboot_common
.global __ax_cpu_x86_boot_3
__ax_cpu_x86_boot_3:
    push 0
    push 3
    jmp .Lboot_common
.global __ax_cpu_x86_boot_4
__ax_cpu_x86_boot_4:
    push 0
    push 4
    jmp .Lboot_common
.global __ax_cpu_x86_boot_5
__ax_cpu_x86_boot_5:
    push 0
    push 5
    jmp .Lboot_common
.global __ax_cpu_x86_boot_6
__ax_cpu_x86_boot_6:
    push 0
    push 6
    jmp .Lboot_common
.global __ax_cpu_x86_boot_7
__ax_cpu_x86_boot_7:
    push 0
    push 7
    jmp .Lboot_common
.global __ax_cpu_x86_boot_8
__ax_cpu_x86_boot_8:
    push 8
    jmp .Lboot_common
.global __ax_cpu_x86_boot_9
__ax_cpu_x86_boot_9:
    push 0
    push 9
    jmp .Lboot_common
.global __ax_cpu_x86_boot_10
__ax_cpu_x86_boot_10:
    push 10
    jmp .Lboot_common
.global __ax_cpu_x86_boot_11
__ax_cpu_x86_boot_11:
    push 11
    jmp .Lboot_common
.global __ax_cpu_x86_boot_12
__ax_cpu_x86_boot_12:
    push 12
    jmp .Lboot_common
.global __ax_cpu_x86_boot_13
__ax_cpu_x86_boot_13:
    push 13
    jmp .Lboot_common
.global __ax_cpu_x86_boot_14
__ax_cpu_x86_boot_14:
    push 14
    jmp .Lboot_common
.global __ax_cpu_x86_boot_15
__ax_cpu_x86_boot_15:
    push 0
    push 15
    jmp .Lboot_common
.global __ax_cpu_x86_boot_16
__ax_cpu_x86_boot_16:
    push 0
    push 16
    jmp .Lboot_common
.global __ax_cpu_x86_boot_17
__ax_cpu_x86_boot_17:
    push 17
    jmp .Lboot_common
.global __ax_cpu_x86_boot_18
__ax_cpu_x86_boot_18:
    push 0
    push 18
    jmp .Lboot_common
.global __ax_cpu_x86_boot_19
__ax_cpu_x86_boot_19:
    push 0
    push 19
    jmp .Lboot_common
.global __ax_cpu_x86_boot_20
__ax_cpu_x86_boot_20:
    push 0
    push 20
    jmp .Lboot_common
.global __ax_cpu_x86_boot_21
__ax_cpu_x86_boot_21:
    push 21
    jmp .Lboot_common
.global __ax_cpu_x86_boot_22
__ax_cpu_x86_boot_22:
    push 0
    push 22
    jmp .Lboot_common
.global __ax_cpu_x86_boot_23
__ax_cpu_x86_boot_23:
    push 0
    push 23
    jmp .Lboot_common
.global __ax_cpu_x86_boot_24
__ax_cpu_x86_boot_24:
    push 0
    push 24
    jmp .Lboot_common
.global __ax_cpu_x86_boot_25
__ax_cpu_x86_boot_25:
    push 0
    push 25
    jmp .Lboot_common
.global __ax_cpu_x86_boot_26
__ax_cpu_x86_boot_26:
    push 0
    push 26
    jmp .Lboot_common
.global __ax_cpu_x86_boot_27
__ax_cpu_x86_boot_27:
    push 0
    push 27
    jmp .Lboot_common
.global __ax_cpu_x86_boot_28
__ax_cpu_x86_boot_28:
    push 0
    push 28
    jmp .Lboot_common
.global __ax_cpu_x86_boot_29
__ax_cpu_x86_boot_29:
    push 29
    jmp .Lboot_common
.global __ax_cpu_x86_boot_30
__ax_cpu_x86_boot_30:
    push 30
    jmp .Lboot_common
.global __ax_cpu_x86_boot_31
__ax_cpu_x86_boot_31:
    push 0
    push 31
    jmp .Lboot_common
.Lboot_common:
    PUSH_GENERAL_REGS
    cld
    mov rdi, rsp
    mov rbx, rsp
    and rsp, -16
    call {dispatch}
    mov rsp, rbx
    POP_GENERAL_REGS
    add rsp, 16
    iretq
