.section .text
.balign 4
.global trap_vector_base
trap_vector_base:
    // LinuxCurrent owns tp as the pinned current-thread header. In kernel
    // mode sscratch is zero; immediately before sret to user it carries the
    // kernel tp while user tp is live. This swap identifies the trap origin
    // without ever treating a userspace value as a CPU-area pointer.
    csrrw   tp, sscratch, tp
    bnez    tp, .Ltrap_from_user

    // Kernel trap: recover the interrupted current header and return
    // sscratch to its canonical zero state before any Rust can execute.
    csrr    tp, sscratch
    csrw    sscratch, zero
    STR     t0, tp, {thread_scratch0_index}
    STR     t1, tp, {thread_scratch1_index}
    LDR     t0, tp, {thread_cpu_base_index}
    addi    sp, sp, -{trapframe_size}
    // PUSH_GENERAL_REGS deliberately skips architectural x0. Initialize that
    // storage before Rust borrows RawTrapFrame as a complete typed value.
    STR     zero, sp, 0
    PUSH_GENERAL_REGS
    LDR     t1, tp, {thread_scratch0_index}
    STR     t1, sp, 5           // tf.regs.t0
    LDR     t1, tp, {thread_scratch1_index}
    STR     t1, sp, 6           // tf.regs.t1
    addi    t1, sp, {trapframe_size}
    STR     t1, sp, 2           // tf.regs.sp
    j       .Ltrap_saved

.Ltrap_from_user:
    // User trap: tp is now the trusted kernel current header and sscratch
    // retains the untrusted user tp until it is copied into UserContext.
    STR     t0, tp, {thread_scratch0_index}
    STR     t1, tp, {thread_scratch1_index}
    LDR     t0, tp, {thread_cpu_base_index}
    LDR     t1, t0, {user_trap_frame_index}
    STR     sp, t1, 2           // tf.regs.sp
    mv      sp, t1
    PUSH_GENERAL_REGS
    csrr    t1, sscratch
    STR     t1, sp, 4           // tf.regs.tp
    csrw    sscratch, zero
    LDR     t1, tp, {thread_scratch0_index}
    STR     t1, sp, 5           // tf.regs.t0
    LDR     t1, tp, {thread_scratch1_index}
    STR     t1, sp, 6           // tf.regs.t1

.Ltrap_saved:
    csrr    t1, sepc
    csrr    t2, sstatus
    STR     t1, sp, 32          // tf.sepc
    STR     t2, sp, 33          // tf.sstatus

    // Kernel Rust always sees the psABI global pointer, trusted kernel tp, and
    // canonical sscratch=0.
    csrw    sscratch, zero
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
    // Return from UserContext::run on the saved kernel continuation. Access
    // CPU state only through the trusted current header recovered into tp.
    LDR     t0, tp, {thread_cpu_base_index}
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

    // Publish both sides of the handoff through the CPU area bound to the
    // trusted kernel current header.
    LDR     t0, tp, {thread_cpu_base_index}
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
    // Kernel return retains canonical sscratch=0 and restores kernel gp/tp.
    csrw    sscratch, zero
    RESTORE_KERNEL_CONTEXT
    j       .Lrestore_context_done
.Lrestore_user_context:
    // Publish trusted kernel tp to sscratch before user tp is restored.
    csrw    sscratch, tp
    RESTORE_USER_CONTEXT
.Lrestore_context_done:
    LDR     sp, sp, 2
    sret
