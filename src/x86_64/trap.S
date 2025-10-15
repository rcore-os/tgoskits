# Reference: <https://github.com/asterinas/asterinas/blob/257b0c63b1f039e1ec4fd94c2c7bd549f8db2830/ostd/src/arch/x86/trap/trap.S>
#            <https://github.com/asterinas/asterinas/blob/257b0c63b1f039e1ec4fd94c2c7bd549f8db2830/ostd/src/arch/x86/trap/syscall.S>

.code64
.equ NUM_INT, 256

.altmacro
.macro DEF_HANDLER, i
.Ltrap_handler_\i:
.if \i == 8 || (\i >= 10 && \i <= 14) || \i == 17 || \i == 21 || \i == 29 || \i == 30
    # error code pushed by CPU
    push    \i          # interrupt vector
    jmp     .Ltrap_common
.else
    push    0           # fill in error code in TrapFrame
    push    \i          # interrupt vector
    jmp     .Ltrap_common
.endif
.endm

.macro DEF_TABLE_ENTRY, i
    .quad .Ltrap_handler_\i
.endm

.section .rodata
.global trap_handler_table
trap_handler_table:
.set i, 0
.rept NUM_INT
    DEF_TABLE_ENTRY %i
    .set i, i + 1
.endr

.macro PUSH_GENERAL_REGS
    push    r15
    push    r14
    push    r13
    push    r12
    push    r11
    push    r10
    push    r9
    push    r8
    push    rdi
    push    rsi
    push    rbp
    push    rbx
    push    rdx
    push    rcx
    push    rax
.endm

.macro POP_GENERAL_REGS
    pop     rax
    pop     rcx
    pop     rdx
    pop     rbx
    pop     rbp
    pop     rsi
    pop     rdi
    pop     r8
    pop     r9
    pop     r10
    pop     r11
    pop     r12
    pop     r13
    pop     r14
    pop     r15
.endm

.section .text
.set i, 0
.rept NUM_INT
    DEF_HANDLER %i
    .set i, i + 1
.endr

.Ltrap_common:
    cld
    test    byte ptr [rsp + 3 * 8], 3
    jz      .Ltrap_kernel

.Ltrap_user:
    swapgs                              # swap in kernel gs
    jmp     .Lexit_user

.Ltrap_kernel:
    PUSH_GENERAL_REGS

    mov     rdi, rsp
    call    x86_trap_handler

    POP_GENERAL_REGS
    add     rsp, 16                     # pop vector, error_code
    iretq

.global syscall_entry
syscall_entry:
    swapgs                              # swap in kernel gs
    mov     gs:[offset __PERCPU_TSS + 12], rsp # store user rsp -> scratch at TSS.sp1
    mov     rsp, gs:[offset __PERCPU_TSS + 4]  # load end of TrapFrame <- TSS.sp0

    push    {UDATA}                     # push ss
    push    gs:[offset __PERCPU_TSS + 12]      # push rsp
    push    r11                         # push rflags
    push    {UCODE64}                   # push cs
    push    rcx                         # push rip

    push    0                           # push error_code
    push    {SYSCALL_VECTOR}            # push vector

.Lexit_user:
    PUSH_GENERAL_REGS

    # restore kernel context
    mov     rsp, [rsp + {trapframe_size}]
    pop     r15
    pop     r14
    pop     r13
    pop     r12
    pop     rbx
    pop     rbp
    ret

.global enter_user
enter_user:
    # save kernel context
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi + {trapframe_size}], rsp

    mov rsp, rdi
    add rdi, {trapframe_size}
    mov gs:[offset __PERCPU_TSS + 4], rdi      # store end of TrapFrame -> TSS.sp0

    swapgs                              # swap in user gs

    POP_GENERAL_REGS
    add rsp, 16                         # pop vector, error_code

    # Determine whether to use sysret or iret.
    # If returning to user space with a clean context,
    # the fast sysret path can be used;
    # otherwise, the slower iret path should be used.
    # Reference: <https://elixir.bootlin.com/linux/v6.0.9/source/arch/x86/entry/entry_64.S#L122>.

    cmp qword ptr [rsp], rcx            # sysret requires rcx = rip
    jne .Liret

    cmp qword ptr [rsp + 16], r11       # sysret requires r11 = rflags
    jne .Liret

    test r11, 0x10100                   # sysret requires rflags not contain RF and TF flags
    jnz .Liret

    shl rcx, 16
    sar rcx, 16
    cmp qword ptr [rsp], rcx            # sysret requires rip be a canonical address
    je .Lsysret
    mov rcx, [rsp]

.Liret:
    iretq

.Lsysret:
    mov rsp, [rsp + 24]                 # load user rsp
    sysretq
