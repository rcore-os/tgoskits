.section .text.__ax_cpu_svm_enter, "ax"
.global __ax_cpu_svm_enter
.hidden __ax_cpu_svm_enter
.type __ax_cpu_svm_enter, @function
__ax_cpu_svm_enter:
    pushfq
    pop qword ptr [rdi + {host_rflags}]
    PUSH_GENERAL_REGS
    mov [rdi + {host_stack_top}], rsp
    XSTATE_ENTER {svm_xstate}
    mov rax, [rdi + {host_vmcb_pa}]
    vmsave rax
    mov rax, rsi
    # Guest FS/GS become live. No Rust code may run before host VMLOAD.
    vmload rax
    sti
    mov rsp, rdi
    POP_GENERAL_REGS 0
    vmrun rax
    cli
    vmsave rax
    mov rax, [rsp + {host_vmcb_from_guest_rsp}]
    vmload rax
    PUSH_GENERAL_REGS 0
    mov rdi, rsp
    XSTATE_EXIT {svm_xstate}
    mov rsp, [rdi + {host_stack_top}]
    POP_GENERAL_REGS
    ret
.size __ax_cpu_svm_enter, . - __ax_cpu_svm_enter
