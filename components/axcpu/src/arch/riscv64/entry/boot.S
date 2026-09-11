// Supervisor boot entry shares native integer save/restore macros.
.section .text.__ax_cpu_riscv_boot_vector, "ax"
.balign 4
.global __ax_cpu_riscv_boot_vector
__ax_cpu_riscv_boot_vector:
    addi sp, sp, -{frame_size}
    sd zero, 0(sp)
    PUSH_GENERAL_REGS
    addi t0, sp, {frame_size}
    sd t0, {sp}(sp)
    csrr t0, sstatus
    sd t0, {status}(sp)
    csrr t0, sepc
    sd t0, {pc}(sp)
    csrr t0, scause
    sd t0, {cause}(sp)
    csrr t0, stval
    sd t0, {value}(sp)
    mv a0, sp
    call {dispatch}
    ld t0, {pc}(sp)
    csrw sepc, t0
    ld t0, {status}(sp)
    csrw sstatus, t0
    RESTORE_KERNEL_CONTEXT
    ld sp, {sp}(sp)
    sret
