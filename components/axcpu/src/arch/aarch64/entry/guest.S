.arch armv8
.macro SAVE_REGS_FROM_EL1
    # Currently `sp` points to the address of `Vcpu.host.stack_top`.
    sub     sp, sp, {trap_frame_size}
    # Currently `sp` points to the base address of `Vcpu.context`, which stores guest's `TrapFrame`.

    # Save general purpose registers into `Vcpu.context`.
    AX_CPU_SAVE_GPRS
    mrs     x9, sp_el0
    str     x9, [sp, {guest_sp_el0_offset}]

    # Save `elr_el2` and `spsr_el2` into `Vcpu.context`.
    mrs     x10, elr_el2
    mrs     x11, spsr_el2
    stp     x10, x11, [sp, {guest_elr_offset}]
.endm

.macro SAVE_VCPU_RUNTIME_FROM_EL1
    # Capture the guest timer, stop it, and restore the host counter domain
    # before any branch-and-link can execute Rust.
    mrs     x9, cntv_ctl_el0
    and     w9, w9, #0x3
    str     w9, [sp, {timer_virtual_control_offset}]
    mrs     x9, cntv_cval_el0
    str     x9, [sp, {timer_virtual_compare_offset}]
    mrs     x9, cntkctl_el1
    str     x9, [sp, {timer_guest_kernel_control_offset}]
    msr     cntv_ctl_el0, xzr
    isb
    ldr     x9, [sp, {timer_host_offset_offset}]
    msr     cntvoff_el2, x9
    ldr     x9, [sp, {timer_host_hypervisor_control_offset}]
    msr     cnthctl_el2, x9
    ldr     x9, [sp, {timer_host_kernel_control_offset}]
    msr     cntkctl_el1, x9
    ldr     x9, [sp, {timer_host_compare_offset}]
    msr     cntv_cval_el0, x9
    isb
    ldr     x9, [sp, {timer_host_control_offset}]
    msr     cntv_ctl_el0, x9
    isb

    # TPIDR_EL0 is shared by host tasks and guests. Save the guest value and
    # restore the host value before any branch-and-link can enter Rust.
    mrs     x9, tpidr_el0
    str     x9, [sp, {guest_tpidr_el0_offset}]
    ldr     x9, [sp, {host_tpidr_el0_offset}]
    msr     tpidr_el0, x9
.endm

.macro SAVE_VCPU_REGS_FROM_EL1
    SAVE_REGS_FROM_EL1
    SAVE_VCPU_RUNTIME_FROM_EL1
.endm

.macro ACK_PENDING_HOST_IRQ
    # CNTV is a level-triggered PPI. Capture the host IAR while the timer is
    # still asserted; SAVE_VCPU_RUNTIME_FROM_EL1 may then stop CNTV without
    # turning the acknowledgement into a spurious INTID. Rust performs the
    # priority drop only after the host timer context has been restored.
    mov     w9, #-1
    str     w9, [sp, {host_pending_irq_ack_offset}]
    ldr     x9, [sp, {host_irq_interface_offset}]
    cmp     x9, #{host_irq_interface_gicv2_mmio}
    b.eq    .Lack_host_irq_gicv2\@
    cmp     x9, #{host_irq_interface_gicv3_sysreg}
    b.ne    .Lack_host_irq_done\@
    mrs     x9, ICC_IAR1_EL1
    str     w9, [sp, {host_pending_irq_ack_offset}]
    dsb     sy
    b       .Lack_host_irq_done\@
.Lack_host_irq_gicv2\@:
    ldr     x10, [sp, {host_irq_cpu_interface_base_offset}]
    ldr     w9, [x10, #0xc]
    str     w9, [sp, {host_pending_irq_ack_offset}]
    dsb     sy
.Lack_host_irq_done\@:
.endm

.macro RESTORE_GUEST_REGS_INTO_EL1
    ldp     x10, x11, [sp, {guest_elr_offset}]
    ldp     x30, x9, [sp, {guest_x30_offset}]
    msr     sp_el0, x9
    msr     elr_el2, x10
    msr     spsr_el2, x11

    ldr     x9, [sp, {guest_tpidr_el0_offset}]
    msr     tpidr_el0, x9
    AX_CPU_RESTORE_GPRS
    add     sp, sp, {trap_frame_size}
.endm

.macro RESTORE_CURRENT_REGS_INTO_EL2
    ldp     x10, x11, [sp, {guest_elr_offset}]
    ldp     x30, x9, [sp, {guest_x30_offset}]
    msr     sp_el0, x9
    msr     elr_el2, x10
    msr     spsr_el2, x11

    AX_CPU_RESTORE_GPRS
    add     sp, sp, {trap_frame_size}
.endm

.macro INVALID_EXCP_CURRENT_EL2, kind, source
    SAVE_REGS_FROM_EL1
    mov     x0, sp
    mov     x1, \kind
    mov     x2, \source
    bl      __ax_cpu_arm_invalid_exception
    b       .Lexception_return_current_el2
.endm

 .macro INVALID_EXCP_LOWER_EL2, kind, source
    SAVE_VCPU_REGS_FROM_EL1
    mov     x0, \kind
    b       __ax_cpu_arm_guest_exit
.endm

.macro HANDLE_CURRENT_IRQ
    SAVE_REGS_FROM_EL1
    mov     x0, sp
    bl      __ax_cpu_arm_current_irq
    b       .Lexception_return_current_el2
.endm

.macro HANDLE_CURRENT_SYNC
    SAVE_REGS_FROM_EL1
    mov     x0, sp
    bl      __ax_cpu_arm_current_sync
    b       .Lexception_return_current_el2
.endm

.macro HANDLE_LOWER_IRQ_VCPU
    SAVE_REGS_FROM_EL1
    ACK_PENDING_HOST_IRQ
    SAVE_VCPU_RUNTIME_FROM_EL1
    mov     x0, {exception_irq}
    bl      __ax_cpu_arm_guest_exit
    # __ax_cpu_arm_guest_exit returns directly to Vcpu::run.
.endm

.macro HANDLE_LOWER_SYNC_VCPU
    SAVE_VCPU_REGS_FROM_EL1
    mov     x0, {exception_sync}
    bl      __ax_cpu_arm_guest_exit
    # __ax_cpu_arm_guest_exit returns directly to Vcpu::run.
.endm

.macro VECTOR_SLOT handler
    b       \handler
    .space  0x80 - 4
.endm

.section .text.ax_cpu_guest,"ax"
.p2align 11
.global __ax_cpu_arm_guest_vector
.hidden __ax_cpu_arm_guest_vector
__ax_cpu_arm_guest_vector:
    // current EL, with SP_EL0
    VECTOR_SLOT .Linvalid_current_sp0_sync
    VECTOR_SLOT .Linvalid_current_sp0_irq
    VECTOR_SLOT .Linvalid_current_sp0_fiq
    VECTOR_SLOT .Linvalid_current_sp0_serror

    // current EL, with SP_ELx
    VECTOR_SLOT .Lhandle_current_spx_sync
    VECTOR_SLOT .Lhandle_current_spx_irq
    VECTOR_SLOT .Linvalid_current_spx_fiq
    VECTOR_SLOT .Linvalid_current_spx_serror

    // lower EL, aarch64
    VECTOR_SLOT .Lhandle_lower_aarch64_sync
    VECTOR_SLOT .Lhandle_lower_aarch64_irq
    VECTOR_SLOT .Linvalid_lower_aarch64_fiq
    VECTOR_SLOT .Linvalid_lower_aarch64_serror

    // lower EL, aarch32
    VECTOR_SLOT .Linvalid_lower_aarch32_sync
    VECTOR_SLOT .Linvalid_lower_aarch32_irq
    VECTOR_SLOT .Linvalid_lower_aarch32_fiq
    VECTOR_SLOT .Linvalid_lower_aarch32_serror

.global __ax_cpu_arm_guest_entry
.hidden __ax_cpu_arm_guest_entry
__ax_cpu_arm_guest_entry:
    # Currently `x0` points to the address of `Vcpu.host.stack_top`.
    mov     sp, x0
    sub     sp, sp, {trap_frame_size}
    # Currently `sp` points to the base address of `Vcpu.context`, which stores guest's `TrapFrame`.
.Lexception_return_guest_el1:
    mrs     x9, cntvoff_el2
    str     x9, [sp, {timer_host_offset_offset}]
    mrs     x9, cntv_cval_el0
    str     x9, [sp, {timer_host_compare_offset}]
    mrs     x9, cntv_ctl_el0
    str     x9, [sp, {timer_host_control_offset}]
    # Save the host timer controls and install the guest direct virtual timer.
    # This is the final assembly-only window before ERET.
    mrs     x9, cnthctl_el2
    str     x9, [sp, {timer_host_hypervisor_control_offset}]
    mrs     x9, cntkctl_el1
    str     x9, [sp, {timer_host_kernel_control_offset}]
    msr     cntv_ctl_el0, xzr
    isb
    ldr     x9, [sp, {timer_virtual_offset_offset}]
    msr     cntvoff_el2, x9
    ldr     x9, [sp, {timer_guest_hypervisor_control_offset}]
    msr     cnthctl_el2, x9
    ldr     x9, [sp, {timer_guest_kernel_control_offset}]
    msr     cntkctl_el1, x9
    ldr     x9, [sp, {timer_virtual_compare_offset}]
    msr     cntv_cval_el0, x9
    isb
    ldr     w9, [sp, {timer_virtual_control_offset}]
    msr     cntv_ctl_el0, x9
    isb
    RESTORE_GUEST_REGS_INTO_EL1
    eret

.Lexception_return_current_el2:
    RESTORE_CURRENT_REGS_INTO_EL2
    eret

.Linvalid_current_sp0_sync:
    INVALID_EXCP_CURRENT_EL2 0 0
.Linvalid_current_sp0_irq:
    INVALID_EXCP_CURRENT_EL2 1 0
.Linvalid_current_sp0_fiq:
    INVALID_EXCP_CURRENT_EL2 2 0
.Linvalid_current_sp0_serror:
    INVALID_EXCP_CURRENT_EL2 3 0

.Lhandle_current_spx_sync:
    HANDLE_CURRENT_SYNC
.Lhandle_current_spx_irq:
    HANDLE_CURRENT_IRQ
.Linvalid_current_spx_fiq:
    INVALID_EXCP_CURRENT_EL2 2 1
.Linvalid_current_spx_serror:
    INVALID_EXCP_CURRENT_EL2 3 1

.Lhandle_lower_aarch64_sync:
    HANDLE_LOWER_SYNC_VCPU
.Lhandle_lower_aarch64_irq:
    HANDLE_LOWER_IRQ_VCPU
.Linvalid_lower_aarch64_fiq:
    INVALID_EXCP_LOWER_EL2 2 2
.Linvalid_lower_aarch64_serror:
    INVALID_EXCP_LOWER_EL2 3 2

.Linvalid_lower_aarch32_sync:
    INVALID_EXCP_LOWER_EL2 0 3
.Linvalid_lower_aarch32_irq:
    INVALID_EXCP_LOWER_EL2 1 3
.Linvalid_lower_aarch32_fiq:
    INVALID_EXCP_LOWER_EL2 2 3
.Linvalid_lower_aarch32_serror:
    INVALID_EXCP_LOWER_EL2 3 3

.global __ax_cpu_arm_run_guest
.hidden __ax_cpu_arm_run_guest
.type __ax_cpu_arm_run_guest, %function
__ax_cpu_arm_run_guest:
    sub     sp, sp, 12 * 8
    stp     x19, x20, [sp]
    stp     x21, x22, [sp, 2 * 8]
    stp     x23, x24, [sp, 4 * 8]
    stp     x25, x26, [sp, 6 * 8]
    stp     x27, x28, [sp, 8 * 8]
    stp     x29, x30, [sp, 10 * 8]
    mov     x19, x0
    mov     x9, sp
    str     x9, [x19, {host_stack_offset}]
    mrs     x9, sp_el0
    str     x9, [x19, {host_sp_el0_offset}]
    mrs     x9, tpidr_el0
    str     x9, [x19, {host_tpidr_el0_offset}]
    mrs     x9, cptr_el2
    str     x9, [x19, {host_cptr_offset}]
    msr     cptr_el2, xzr
    isb
    add     x0, x19, {host_fp_offset}
    bl      {fp_save}
    add     x0, x19, {guest_fp_offset}
    bl      {fp_restore}
    add     x0, x19, {host_stack_offset}
    b       __ax_cpu_arm_guest_entry
.size __ax_cpu_arm_run_guest, . - __ax_cpu_arm_run_guest

.type __ax_cpu_arm_guest_exit, %function
__ax_cpu_arm_guest_exit:
    str     x0, [sp, {exit_kind_offset}]
    mrs     x9, esr_el2
    str     x9, [sp, {exit_syndrome_offset}]
    mrs     x9, far_el2
    str     x9, [sp, {exit_fault_offset}]
    mrs     x9, hpfar_el2
    str     x9, [sp, {exit_physical_fault_offset}]
    mrs     x9, elr_el2
    str     x9, [sp, {exit_pc_offset}]
    mov     x19, sp
    add     x0, x19, {guest_fp_offset}
    bl      {fp_save}
    add     x0, x19, {host_fp_offset}
    bl      {fp_restore}
    ldr     x9, [x19, {host_cptr_offset}]
    msr     cptr_el2, x9
    isb
    ldr     x9, [x19, {host_sp_el0_offset}]
    msr     sp_el0, x9
    ldr     x9, [x19, {host_stack_offset}]
    mov     sp, x9
    ldp     x19, x20, [sp]
    ldp     x21, x22, [sp, 2 * 8]
    ldp     x23, x24, [sp, 4 * 8]
    ldp     x25, x26, [sp, 6 * 8]
    ldp     x27, x28, [sp, 8 * 8]
    ldp     x29, x30, [sp, 10 * 8]
    add     sp, sp, 12 * 8
    ret
.size __ax_cpu_arm_guest_exit, . - __ax_cpu_arm_guest_exit
