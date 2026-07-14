.section .text
.balign 4
.global trap_vector_base
trap_vector_base:
    // `sscratch` always contains the current CpuAreaPrefix address. Preserve
    // the interrupted temporaries in the header before using them to select a
    // stack from sstatus.SPP.
    csrrw   t0, sscratch, t0
    STR     t1, t0, {entry_scratch1_index}
    csrr    t1, sscratch
    STR     t1, t0, {entry_scratch0_index}
    csrw    sscratch, t0

    csrr    t1, sstatus
    andi    t1, t1, 1 << 8      // sstatus.SPP == 1: trap from S mode
    beqz    t1, .Ltrap_from_user

    // Kernel traps stay on the interrupted kernel stack.
    addi    sp, sp, -{trapframe_size}
    PUSH_GENERAL_REGS
    LDR     t1, t0, {entry_scratch0_index}
    STR     t1, sp, 5           // tf.regs.t0
    LDR     t1, t0, {entry_scratch1_index}
    STR     t1, sp, 6           // tf.regs.t1
    addi    t1, sp, {trapframe_size}
    STR     t1, sp, 2           // tf.regs.sp
    j       .Ltrap_saved

.Ltrap_from_user:
    // User traps save directly into the UserContext published by enter_user.
    // Record user sp before replacing it with the frame pointer.
    LDR     t1, t0, {user_trap_frame_index}
    STR     sp, t1, 2           // tf.regs.sp
    mv      sp, t1
    PUSH_GENERAL_REGS
    LDR     t1, t0, {entry_scratch0_index}
    STR     t1, sp, 5           // tf.regs.t0
    LDR     t1, t0, {entry_scratch1_index}
    STR     t1, sp, 6           // tf.regs.t1

.Ltrap_saved:
    csrr    t1, sepc
    csrr    t2, sstatus
    STR     t1, sp, 32          // tf.sepc
    STR     t2, sp, 33          // tf.sstatus

    // Rust may use CPU-local state and kernel statics immediately. Reassert
    // both architecture anchors after the frame is complete and before any
    // Rust instruction can execute.
    csrw    sscratch, t0
    .option push
    .option norelax
    lla     gp, __global_pointer$
    .option pop

    andi    t2, t2, 1 << 8      // sstatus.SPP == 1
    beqz    t2, .Lexit_user

    mv      a0, sp
    la      ra, .Ltrap_return
    j       riscv_trap_handler

.Lexit_user:
    // Return from UserContext::run on the kernel continuation stack. Its
    // callee-saved image includes the kernel gp and task-owned kernel tp.
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

    // Publish both sides of the user/kernel stack handoff while IRQs are
    // disabled. `sscratch` itself remains the CPU-area anchor in user mode.
    csrr    t0, sscratch
    STR     sp, t0, {kernel_stack_pointer_index}
    STR     a0, t0, {user_trap_frame_index}
    mv      sp, a0

.Ltrap_return:
    LDR     t0, sp, 32
    LDR     t1, sp, 33
    csrw    sepc, t0
    csrw    sstatus, t1

    // Both privilege origins restore their own architectural gp/tp image.
    // Kernel gp is the standard global pointer, while tp is task-owned TLS.
    POP_GENERAL_REGS
    LDR     sp, sp, 2           // restore interrupted sp

    sret
