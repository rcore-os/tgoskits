.macro _asm_nofault_extable from, to
    .pushsection __nofault_ex_table, "a"
    .balign 4
    .word \from - .
    .word \to - .
    .popsection
.endm

.section .text
.global __axcpu_user_read_u32
.type __axcpu_user_read_u32, %function
__axcpu_user_read_u32:
1:
    ldr w2, [x0]
    str w2, [x1]
    mov w0, #0
    ret

.Luser_read_fault:
    mov w0, #1
    ret

    _asm_nofault_extable 1b, .Luser_read_fault
.size __axcpu_user_read_u32, . - __axcpu_user_read_u32

.global __axcpu_user_atomic_u32
.type __axcpu_user_atomic_u32, %function
__axcpu_user_atomic_u32:
    mov w7, #128
1:
    ldxr w4, [x0]
    cmp w1, #0
    b.eq .Luser_atomic_set
    cmp w1, #1
    b.eq .Luser_atomic_add
    cmp w1, #2
    b.eq .Luser_atomic_or
    cmp w1, #3
    b.eq .Luser_atomic_and_not
    eor w5, w4, w2
    b 2f
.Luser_atomic_set:
    mov w5, w2
    b 2f
.Luser_atomic_add:
    add w5, w4, w2
    b 2f
.Luser_atomic_or:
    orr w5, w4, w2
    b 2f
.Luser_atomic_and_not:
    bic w5, w4, w2
2:
    stlxr w6, w5, [x0]
    cbz w6, .Luser_atomic_success
    subs w7, w7, #1
    b.ne 1b
    mov w0, #2
    dmb ish
    ret

.Luser_atomic_success:
    dmb ish
    str w4, [x3]
    mov w0, #0
    ret

.Luser_atomic_fault:
    mov w0, #1
    ret

    _asm_nofault_extable 1b, .Luser_atomic_fault
    _asm_nofault_extable 2b, .Luser_atomic_fault
.size __axcpu_user_atomic_u32, . - __axcpu_user_atomic_u32
