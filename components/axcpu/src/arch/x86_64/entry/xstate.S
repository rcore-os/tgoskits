# Shared VMX/SVM extended-state window. R8 owns the descriptor; RDI and RSI
# retain the entry context and SVM guest address. Only saved scratch GPRs change.
.macro XS_READ_MSR index, slot
    mov ecx, \index
    rdmsr
    shl rdx, 32
    or rax, rdx
    mov [r8 + \slot], rax
.endm
.macro XS_WRITE_MSR index, slot
    mov ecx, \index
    mov rax, [r8 + \slot]
    mov rdx, rax
    shr rdx, 32
    wrmsr
.endm
.macro XS_SET_XCR0 slot
    xor ecx, ecx
    mov rax, [r8 + \slot]
    mov rdx, rax
    shr rdx, 32
    xsetbv
.endm
.macro XS_READ_BANKS xcr0, xss, xfd, error
    xor ecx, ecx
    xgetbv
    shl rdx, 32
    or rax, rdx
    mov [r8 + \xcr0], rax
    cmp qword ptr [r8 + {xs_mode}], 2
    jne .Lxs_no_xss_\@
    XS_READ_MSR 0xda0, \xss
.Lxs_no_xss_\@:
    cmp qword ptr [r8 + {xs_has_xfd}], 0
    je .Lxs_no_xfd_\@
    XS_READ_MSR 0x1c4, \xfd
    XS_READ_MSR 0x1c5, \error
    # Retain dormant components as well as those enabled by the current bank.
    mov ecx, 0x1c4
    xor eax, eax
    xor edx, edx
    wrmsr
.Lxs_no_xfd_\@:
.endm
.macro XS_SET_BANKS xcr0, xss, xfd, error
    XS_SET_XCR0 \xcr0
    cmp qword ptr [r8 + {xs_mode}], 2
    jne .Lxs_set_no_xss_\@
    XS_WRITE_MSR 0xda0, \xss
.Lxs_set_no_xss_\@:
    cmp qword ptr [r8 + {xs_has_xfd}], 0
    je .Lxs_set_no_xfd_\@
    XS_WRITE_MSR 0x1c4, \xfd
    XS_WRITE_MSR 0x1c5, \error
.Lxs_set_no_xfd_\@:
.endm
.macro XS_FULL_BANKS
    XS_SET_XCR0 {xs_full_xcr0}
    cmp qword ptr [r8 + {xs_mode}], 2
    jne .Lxs_full_no_xss_\@
    XS_WRITE_MSR 0xda0, {xs_full_xss}
.Lxs_full_no_xss_\@:
.endm
.macro XS_IMAGE save, restore
    mov rax, [r8 + {xs_full_xcr0}]
    or rax, [r8 + {xs_full_xss}]
    mov rdx, rax
    shr rdx, 32
    mov r9, [r8 + \save]
    cmp qword ptr [r8 + {xs_mode}], 2
    je .Lxs_compact_\@
    xsave64 [r9]
    mov r9, [r8 + \restore]
    xrstor64 [r9]
    jmp .Lxs_image_done_\@
.Lxs_compact_\@:
    xsaves64 [r9]
    mov r9, [r8 + \restore]
    xrstors64 [r9]
.Lxs_image_done_\@:
.endm
.macro XSTATE_ENTER pointer
    mov r8, [rdi + \pointer]
    cmp qword ptr [r8 + {xs_mode}], 0
    je .Lxs_enter_fx_\@
    XS_READ_BANKS {xs_host_xcr0}, {xs_host_xss}, {xs_host_xfd}, {xs_host_xfd_err}
    XS_FULL_BANKS
    XS_IMAGE {xs_host}, {xs_guest}
    XS_SET_BANKS {xs_guest_xcr0}, {xs_guest_xss}, {xs_guest_xfd}, {xs_guest_xfd_err}
    jmp .Lxs_enter_done_\@
.Lxs_enter_fx_\@:
    mov r9, [r8 + {xs_host}]
    fxsave64 [r9]
    mov r9, [r8 + {xs_guest}]
    fxrstor64 [r9]
.Lxs_enter_done_\@:
.endm
.macro XSTATE_EXIT pointer
    mov r8, [rdi + \pointer]
    cmp qword ptr [r8 + {xs_mode}], 0
    je .Lxs_exit_fx_\@
    XS_READ_BANKS {xs_guest_xcr0}, {xs_guest_xss}, {xs_guest_xfd}, {xs_guest_xfd_err}
    XS_FULL_BANKS
    XS_IMAGE {xs_guest}, {xs_host}
    XS_SET_BANKS {xs_host_xcr0}, {xs_host_xss}, {xs_host_xfd}, {xs_host_xfd_err}
    jmp .Lxs_exit_done_\@
.Lxs_exit_fx_\@:
    mov r9, [r8 + {xs_guest}]
    fxsave64 [r9]
    mov r9, [r8 + {xs_host}]
    fxrstor64 [r9]
.Lxs_exit_done_\@:
.endm
