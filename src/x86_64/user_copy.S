.macro _asm_extable, from, to
    .pushsection __ex_table, "a"
    .balign 8
    .quad \from
    .quad \to
    .popsection
.endm

.section .text
.global user_copy
user_copy:
    // Arguments: rdi (dst), rsi (src), rdx (size)
    mov rcx, rdx        // copy size to rcx
    cld                 // clear direction flag(DF=0) for forward direction
    // copy rcx bytes from [rsi] to [rdi]
0:  rep movsb           // copy bytes from src to dst
1:  mov rax, rcx        // return remain bytes
    ret

    _asm_extable 0b, 1b
