.macro SAVE_REGS_FROM_EL1
    # Currently `sp` points to the address of `ArmVcpu.host.stack_top`.
    sub     sp, sp, {trap_frame_size}
    # Currently `sp` points to the base address of `ArmVcpu.ctx`, which stores guest's `TrapFrame`.

    # Save general purpose registers into `ArmVcpu.ctx`.
    stp     x0, x1, [sp]
    stp     x2, x3, [sp, 2 * 8]
    stp     x4, x5, [sp, 4 * 8]
    stp     x6, x7, [sp, 6 * 8]
    stp     x8, x9, [sp, 8 * 8]
    stp     x10, x11, [sp, 10 * 8]
    stp     x12, x13, [sp, 12 * 8]
    stp     x14, x15, [sp, 14 * 8]
    stp     x16, x17, [sp, 16 * 8]
    stp     x18, x19, [sp, 18 * 8]
    stp     x20, x21, [sp, 20 * 8]
    stp     x22, x23, [sp, 22 * 8]
    stp     x24, x25, [sp, 24 * 8]
    stp     x26, x27, [sp, 26 * 8]
    stp     x28, x29, [sp, 28 * 8]

    mrs     x9, sp_el0
    stp     x30, x9, [sp, 30 * 8]

    # Save `elr_el2` and `spsr_el2` into `ArmVcpu.ctx`.
    mrs     x10, elr_el2
    mrs     x11, spsr_el2
    stp     x10, x11, [sp, 32 * 8]
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
    msr     cntvoff_el2, xzr
    ldr     x9, [sp, {timer_host_hypervisor_control_offset}]
    msr     cnthctl_el2, x9
    ldr     x9, [sp, {timer_host_kernel_control_offset}]
    msr     cntkctl_el1, x9
    strb    wzr, [sp, {timer_loaded_offset}]
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
    ldp     x10, x11, [sp, 32 * 8]
    ldp     x30, x9, [sp, 30 * 8]
    msr     sp_el0, x9
    msr     elr_el2, x10
    msr     spsr_el2, x11

    ldr     x30,      [sp, 30 * 8]
    ldp     x28, x29, [sp, 28 * 8]
    ldp     x26, x27, [sp, 26 * 8]
    ldp     x24, x25, [sp, 24 * 8]
    ldp     x22, x23, [sp, 22 * 8]
    ldp     x20, x21, [sp, 20 * 8]
    ldp     x18, x19, [sp, 18 * 8]
    ldp     x16, x17, [sp, 16 * 8]
    ldp     x14, x15, [sp, 14 * 8]
    ldp     x12, x13, [sp, 12 * 8]
    ldp     x10, x11, [sp, 10 * 8]
    # Install guest TLS only in the final assembly window. The following
    # instructions restore guest GPRs and return directly; no helper runs.
    ldr     x9, [sp, {guest_tpidr_el0_offset}]
    msr     tpidr_el0, x9
    ldp     x8, x9, [sp, 8 * 8]
    ldp     x6, x7, [sp, 6 * 8]
    ldp     x4, x5, [sp, 4 * 8]
    ldp     x2, x3, [sp, 2 * 8]
    ldp     x0, x1, [sp]
    add     sp, sp, {trap_frame_size}
.endm

.macro RESTORE_CURRENT_REGS_INTO_EL2
    ldp     x10, x11, [sp, 32 * 8]
    ldp     x30, x9, [sp, 30 * 8]
    msr     sp_el0, x9
    msr     elr_el2, x10
    msr     spsr_el2, x11

    ldr     x30,      [sp, 30 * 8]
    ldp     x28, x29, [sp, 28 * 8]
    ldp     x26, x27, [sp, 26 * 8]
    ldp     x24, x25, [sp, 24 * 8]
    ldp     x22, x23, [sp, 22 * 8]
    ldp     x20, x21, [sp, 20 * 8]
    ldp     x18, x19, [sp, 18 * 8]
    ldp     x16, x17, [sp, 16 * 8]
    ldp     x14, x15, [sp, 14 * 8]
    ldp     x12, x13, [sp, 12 * 8]
    ldp     x10, x11, [sp, 10 * 8]
    ldp     x8, x9, [sp, 8 * 8]
    ldp     x6, x7, [sp, 6 * 8]
    ldp     x4, x5, [sp, 4 * 8]
    ldp     x2, x3, [sp, 2 * 8]
    ldp     x0, x1, [sp]
    add     sp, sp, {trap_frame_size}
.endm

.macro INVALID_EXCP_CURRENT_EL2, kind, source
    SAVE_REGS_FROM_EL1
    mov     x0, sp
    mov     x1, \kind
    mov     x2, \source
    bl      invalid_exception_el2
    b       .Lexception_return_current_el2
.endm

.macro INVALID_EXCP_LOWER_EL2, kind, source
    SAVE_VCPU_REGS_FROM_EL1
    mov     x0, sp
    mov     x1, \kind
    mov     x2, \source
    bl      invalid_exception_el2
    b       .Lexception_return_guest_el1
.endm

.macro HANDLE_CURRENT_IRQ
    SAVE_REGS_FROM_EL1
    mov     x0, sp
    bl      current_el_irq_handler
    b       .Lexception_return_current_el2
.endm

.macro HANDLE_CURRENT_SYNC
    SAVE_REGS_FROM_EL1
    mov     x0, sp
    bl      current_el_sync_handler
    b       .Lexception_return_current_el2
.endm

.macro HANDLE_LOWER_IRQ_VCPU
    SAVE_REGS_FROM_EL1
    ACK_PENDING_HOST_IRQ
    SAVE_VCPU_RUNTIME_FROM_EL1
    mov     x0, {exception_irq}
    bl      vmexit_trampoline
    # vmexit_trampoline returns directly to ArmVcpu::run.
.endm

.macro HANDLE_LOWER_SYNC_VCPU
    SAVE_VCPU_REGS_FROM_EL1
    mov     x0, {exception_sync}
    bl      vmexit_trampoline
    # vmexit_trampoline returns directly to ArmVcpu::run.
.endm

.macro VECTOR_SLOT handler
    b       \handler
    .space  0x80 - 4
.endm

.section .text
.p2align 11
.global exception_vector_base_vcpu
exception_vector_base_vcpu:
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

.global context_vm_entry
context_vm_entry:
    # Currently `x0` points to the address of `ArmVcpu.host.stack_top`.
    mov     sp, x0
    sub     sp, sp, {trap_frame_size}
    # Currently `sp` points to the base address of `ArmVcpu.ctx`, which stores guest's `TrapFrame`.
.Lexception_return_guest_el1:
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
    mov     w9, #1
    strb    w9, [sp, {timer_loaded_offset}]
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
