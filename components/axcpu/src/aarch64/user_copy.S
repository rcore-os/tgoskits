// Ref: https://github.com/torvalds/linux/blob/v6.16/arch/arm64/lib/copy_from_user.S + project exception table style
// Implements: size_t user_copy(void *dst, const void *src, size_t size)
// Arguments: x0=dst, x1=src, x2=size
// Returns: 0 on success; remaining bytes (not copied) if a data abort occurs

.macro _asm_extable, from, to
    .pushsection __ex_table, "a"
    .balign 8
    .quad \from
    .quad \to
    .popsection
.endm

.section .text
.global user_copy

// Strategy:
// 1. Fast path: if size==0 return 0
// 2. Align dst to 8 bytes with byte copies (safe, faultable loads)
// 3. Bulk copy 64 bytes per iteration using 8x LDP/STP pairs
// 4. Copy remaining 8-byte chunks
// 5. Tail copy bytes
// All load/store pairs are covered by exception table entries pointing to .Lfault
// so that on fault we compute remaining = original_end - current_dst.

user_copy:
    cbz     x2, .Lsuccess            // nothing to do
    add     x3, x0, x2                // x3 = dst_end (for remaining calc)
    mov     x4, x2                    // save original remaining (debug/unused)

    // Align destination to 8 bytes (copy a few leading bytes)
    and     x5, x0, #7                // misalignment
    cbz     x5, .Ldst_aligned
    mov     x6, #8
    sub     x5, x6, x5                // bytes needed to align
    cmp     x2, x5
    csel    x5, x2, x5, lo            // if size < needed, only copy size
.Lalign_loop:
    cbz     x5, .Ldst_aligned
1:  ldrb    w6, [x1], #1              // may fault
2:  strb    w6, [x0], #1
    subs    x5, x5, #1
    subs    x2, x2, #1
    b.ne    .Lalign_loop
    cbz     x2, .Lsuccess

.Ldst_aligned:
    // Bulk 64-byte loop if enough bytes
    cmp     x2, #64
    b.lo    .Lword_tail

    // x2 >= 64
.Lbulk_loop:
3:  ldp     x6,  x7,  [x1], #16       // 16 bytes
4:  ldp     x8,  x9,  [x1], #16
5:  ldp     x10, x11, [x1], #16
6:  ldp     x12, x13, [x1], #16
7:  stp     x6,  x7,  [x0], #16
8:  stp     x8,  x9,  [x0], #16
9:  stp     x10, x11, [x0], #16
10: stp     x12, x13, [x0], #16
    subs    x2, x2, #64
    cmp     x2, #64
    b.hs    .Lbulk_loop
    cbz     x2, .Lsuccess

.Lword_tail:
    // Copy remaining 8-byte words
    cmp     x2, #8
    b.lo    .Lbyte_tail
.Lword_tail_loop:
11: ldr     x6, [x1], #8
12: str     x6, [x0], #8
    subs    x2, x2, #8
    cmp     x2, #8
    b.hs    .Lword_tail_loop
    cbz     x2, .Lsuccess

.Lbyte_tail:
    // Copy leftover bytes
.Lbyte_tail_loop:
13: ldrb    w6, [x1], #1
14: strb    w6, [x0], #1
    subs    x2, x2, #1
    b.ne    .Lbyte_tail_loop

.Lsuccess:
    mov     x0, #0                    // success
    ret

// Fault handler: x0 currently points just past last successfully written byte.
// Remaining = dst_end - current_dst (x3 - x0)
.Lfault:
    sub     x0, x3, x0
    ret

// Exception table entries for every faultable memory access.
    _asm_extable 1b, .Lfault
    _asm_extable 2b, .Lfault
    _asm_extable 3b, .Lfault
    _asm_extable 4b, .Lfault
    _asm_extable 5b, .Lfault
    _asm_extable 6b, .Lfault
    _asm_extable 7b, .Lfault
    _asm_extable 8b, .Lfault
    _asm_extable 9b, .Lfault
    _asm_extable 10b, .Lfault
    _asm_extable 11b, .Lfault
    _asm_extable 12b, .Lfault
    _asm_extable 13b, .Lfault
    _asm_extable 14b, .Lfault
