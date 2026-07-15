.section .text
.balign 4
.global trap_vector_base
trap_vector_base:
    // UnikernelTls keeps the CPU-area prefix in sscratch and task TLS in tp.
    // Preserve interrupted temporaries in the CPU-owned runtime anchor before
    // selecting a stack from sstatus.SPP.
    csrrw   t0, sscratch, t0
    STR     t1, t0, {entry_scratch1_index}
    csrr    t1, sscratch
    STR     t1, t0, {entry_scratch0_index}
    csrw    sscratch, t0

    csrr    t1, sstatus
    andi    t1, t1, 1 << 8
    beqz    t1, .Ltrap_from_user

    addi    sp, sp, -{trapframe_size}
    PUSH_GENERAL_REGS
    LDR     t1, t0, {entry_scratch0_index}
    STR     t1, sp, 5
    LDR     t1, t0, {entry_scratch1_index}
    STR     t1, sp, 6
    addi    t1, sp, {trapframe_size}
    STR     t1, sp, 2
    j       .Ltrap_saved

.Ltrap_from_user:
    LDR     t1, t0, {user_trap_frame_index}
    STR     sp, t1, 2
    mv      sp, t1
    PUSH_GENERAL_REGS
    LDR     t1, t0, {entry_scratch0_index}
    STR     t1, sp, 5
    LDR     t1, t0, {entry_scratch1_index}
    STR     t1, sp, 6

.Ltrap_saved:
    csrr    t1, sepc
    csrr    t2, sstatus
    STR     t1, sp, 32
    STR     t2, sp, 33
    csrw    sscratch, t0
    .option push
    .option norelax
    lla     gp, __global_pointer$
    .option pop

    andi    t2, t2, 1 << 8
    beqz    t2, .Lexit_user
    mv      a0, sp
    la      ra, .Ltrap_return
    j       riscv_trap_handler

.Lexit_user:
    csrr    t0, sscratch
    LDR     sp, t0, {kernel_stack_pointer_index}
    LDR     s0, sp, 0
    LDR     s1, sp, 1
    LDR     s2, sp, 2
    LDR     s3, sp, 3
    LDR     s4, sp, 4
    LDR     s5, sp, 5
    LDR     s6, sp, 6
    LDR     s7, sp, 7
    LDR     s8, sp, 8
    LDR     s9, sp, 9
    LDR     s10, sp, 10
    LDR     s11, sp, 11
    LDR     ra, sp, 12
    LDR     gp, sp, 13
    LDR     tp, sp, 14
    addi    sp, sp, 16 * XLENB
    ret

.global enter_user
enter_user:
    addi    sp, sp, -16 * XLENB
    STR     s0, sp, 0
    STR     s1, sp, 1
    STR     s2, sp, 2
    STR     s3, sp, 3
    STR     s4, sp, 4
    STR     s5, sp, 5
    STR     s6, sp, 6
    STR     s7, sp, 7
    STR     s8, sp, 8
    STR     s9, sp, 9
    STR     s10, sp, 10
    STR     s11, sp, 11
    STR     ra, sp, 12
    STR     gp, sp, 13
    STR     tp, sp, 14
    csrr    t0, sscratch
    STR     sp, t0, {kernel_stack_pointer_index}
    STR     a0, t0, {user_trap_frame_index}
    mv      sp, a0

.Ltrap_return:
    LDR     t0, sp, 32
    LDR     t1, sp, 33
    csrw    sepc, t0
    csrw    sstatus, t1
    andi    t2, t1, 1 << 8
    beqz    t2, .Lrestore_user_context
    RESTORE_KERNEL_CONTEXT
    j       .Lrestore_context_done
.Lrestore_user_context:
    RESTORE_USER_CONTEXT
.Lrestore_context_done:
    LDR     sp, sp, 2
    sret
