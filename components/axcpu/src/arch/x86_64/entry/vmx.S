.section .text.__ax_cpu_vmx_entry, "ax"
.macro VMX_SWAP_MSR msr, field, save, restore
    mov ecx, \msr
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [rdi + \save + \field], rax
    mov rax, [rdi + \restore + \field]
    mov rdx, rax
    shr rdx, 32
    wrmsr
.endm
.macro VMX_SWAP_SYSCALL save, restore
    VMX_SWAP_MSR 0xc0000081, {vmx_syscall_star}, \save, \restore
    VMX_SWAP_MSR 0xc0000082, {vmx_syscall_lstar}, \save, \restore
    VMX_SWAP_MSR 0xc0000083, {vmx_syscall_cstar}, \save, \restore
    VMX_SWAP_MSR 0xc0000084, {vmx_syscall_fmask}, \save, \restore
    VMX_SWAP_MSR 0xc0000102, {vmx_syscall_kernel_gs_base}, \save, \restore
.endm
.macro VMX_ENTER name, instruction
.global \name
.hidden \name
.type \name, @function
\name:
    pushfq
    pop qword ptr [rdi + {vmx_host_rflags}]
    PUSH_GENERAL_REGS
    mov [rdi + {vmx_host_stack_top}], rsp
    mov rax, cr2
    mov [rdi + {vmx_host_cr2}], rax
    mov rax, [rdi + {vmx_guest_cr2}]
    mov cr2, rax
    XSTATE_ENTER {vmx_xstate}
    VMX_SWAP_SYSCALL {vmx_host_syscall}, {vmx_guest_syscall}
    mov rsp, rdi
    POP_GENERAL_REGS
    \instruction
    # Capture VMfail flags before any arithmetic changes them. The stack is
    # still the context's host-stack slot; return through the saved host frame.
    setc byte ptr [rsp + {vmx_failure_from_guest_rsp}]
    setz byte ptr [rsp + {vmx_failure_from_guest_rsp} + 1]
    lea rdi, [rsp - {vmx_gpr_size}]
    XSTATE_EXIT {vmx_xstate}
    VMX_SWAP_SYSCALL {vmx_guest_syscall}, {vmx_host_syscall}
    mov rax, [rdi + {vmx_host_cr2}]
    mov cr2, rax
    mov rsp, [rsp + {vmx_stack_slot_from_guest_rsp}]
    POP_GENERAL_REGS
    ret
.size \name, . - \name
.endm
VMX_ENTER __ax_cpu_vmx_launch, vmlaunch
VMX_ENTER __ax_cpu_vmx_resume, vmresume
.purgem VMX_ENTER

.global __ax_cpu_vmx_exit
.hidden __ax_cpu_vmx_exit
.type __ax_cpu_vmx_exit, @function
__ax_cpu_vmx_exit:
    cli
    PUSH_GENERAL_REGS
    mov rax, cr2
    mov [rsp + {vmx_guest_cr2}], rax
    mov rax, [rsp + {vmx_host_cr2}]
    mov cr2, rax
    mov rdi, rsp
    XSTATE_EXIT {vmx_xstate}
    VMX_SWAP_SYSCALL {vmx_guest_syscall}, {vmx_host_syscall}
    mov rsp, [rsp + {vmx_host_stack_top}]
    POP_GENERAL_REGS
    ret
.size __ax_cpu_vmx_exit, . - __ax_cpu_vmx_exit

.purgem VMX_SWAP_SYSCALL
.purgem VMX_SWAP_MSR
