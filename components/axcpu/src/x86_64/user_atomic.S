.macro _asm_nofault_extable from, to
    .pushsection __nofault_ex_table, "a"
    .balign 4
    .long \from - .
    .long \to - .
    .popsection
.endm

.section .text
.global __axcpu_user_read_u32
.type __axcpu_user_read_u32, @function
__axcpu_user_read_u32:
.Luser_read:
    mov eax, dword ptr [rdi]
    mov dword ptr [rsi], eax
    xor eax, eax
    ret

.Luser_read_fault:
    mov eax, 1
    ret

    _asm_nofault_extable .Luser_read, .Luser_read_fault
.size __axcpu_user_read_u32, . - __axcpu_user_read_u32

.global __axcpu_user_atomic_u32
.type __axcpu_user_atomic_u32, @function
__axcpu_user_atomic_u32:
    mov r8, rcx
    cmp esi, 0
    je .Luser_atomic_set
    cmp esi, 1
    je .Luser_atomic_add
    cmp esi, 2
    je .Luser_atomic_or
    cmp esi, 3
    je .Luser_atomic_and_not

    // The typed Rust API guarantees that the remaining operation is XOR.
    mov r10d, 128
.Luser_atomic_xor_load:
    mov eax, dword ptr [rdi]
.Luser_atomic_xor_retry:
    mov r9d, eax
    xor r9d, edx
.Luser_atomic_xor_cmpxchg:
    lock cmpxchg dword ptr [rdi], r9d
    jz .Luser_atomic_success
    dec r10d
    jnz .Luser_atomic_xor_retry
    jmp .Luser_atomic_retry

.Luser_atomic_set:
    mov eax, edx
.Luser_atomic_set_xchg:
    xchg dword ptr [rdi], eax
    jmp .Luser_atomic_success

.Luser_atomic_add:
    mov eax, edx
.Luser_atomic_add_xadd:
    lock xadd dword ptr [rdi], eax
    jmp .Luser_atomic_success

.Luser_atomic_or:
    mov r10d, 128
.Luser_atomic_or_load:
    mov eax, dword ptr [rdi]
.Luser_atomic_or_retry:
    mov r9d, eax
    or r9d, edx
.Luser_atomic_or_cmpxchg:
    lock cmpxchg dword ptr [rdi], r9d
    jz .Luser_atomic_success
    dec r10d
    jnz .Luser_atomic_or_retry
    jmp .Luser_atomic_retry

.Luser_atomic_and_not:
    not edx
    mov r10d, 128
.Luser_atomic_and_not_load:
    mov eax, dword ptr [rdi]
.Luser_atomic_and_not_retry:
    mov r9d, eax
    and r9d, edx
.Luser_atomic_and_not_cmpxchg:
    lock cmpxchg dword ptr [rdi], r9d
    jz .Luser_atomic_success
    dec r10d
    jnz .Luser_atomic_and_not_retry
    jmp .Luser_atomic_retry

.Luser_atomic_success:
    mov dword ptr [r8], eax
    xor eax, eax
    ret

.Luser_atomic_fault:
    mov eax, 1
    ret

.Luser_atomic_retry:
    mov eax, 2
    ret

    _asm_nofault_extable .Luser_atomic_xor_load, .Luser_atomic_fault
    _asm_nofault_extable .Luser_atomic_xor_cmpxchg, .Luser_atomic_fault
    _asm_nofault_extable .Luser_atomic_set_xchg, .Luser_atomic_fault
    _asm_nofault_extable .Luser_atomic_add_xadd, .Luser_atomic_fault
    _asm_nofault_extable .Luser_atomic_or_load, .Luser_atomic_fault
    _asm_nofault_extable .Luser_atomic_or_cmpxchg, .Luser_atomic_fault
    _asm_nofault_extable .Luser_atomic_and_not_load, .Luser_atomic_fault
    _asm_nofault_extable .Luser_atomic_and_not_cmpxchg, .Luser_atomic_fault
.size __axcpu_user_atomic_u32, . - __axcpu_user_atomic_u32
