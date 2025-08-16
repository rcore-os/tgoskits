// Ref: https://elixir.bootlin.com/linux/v6.16/source/arch/riscv/lib/uaccess.S

.macro fixup op reg addr lbl
100:
	\op \reg, \addr
	_asm_extable	100b, \lbl
.endm

.section .text
.global user_copy
user_copy:
	/*
	 * Save the terminal address which will be used to compute the number
	 * of bytes copied in case of a fixup exception.
	 */
	add	t5, a0, a2

	/*
	 * Register allocation for code below:
	 * a0 - start of uncopied dst
	 * a1 - start of uncopied src
	 * a2 - size
	 * t0 - end of uncopied dst
	 */
	add	t0, a0, a2

	/*
	 * Use byte copy only if too small.
	 * XLENB holds 4 for RV32 and 8 for RV64
	 */
	li	a3, 9*XLENB-1 /* size must >= (word_copy REG_Side + XLENB-1) */
	bltu	a2, a3, .Lbyte_copy_tail

	/*
	 * Copy first bytes until dst is aligned to word boundary.
	 * a0 - start of dst
	 * t1 - start of aligned dst
	 */
	addi	t1, a0, XLENB-1
	andi	t1, t1, ~(XLENB-1)
	/* dst is already aligned, skip */
	beq	a0, t1, .Lskip_align_dst
1:
	/* a5 - one byte for copying data */
	fixup lb      a5, 0(a1), 10f
	addi	a1, a1, 1	/* src */
	fixup sb      a5, 0(a0), 10f
	addi	a0, a0, 1	/* dst */
	bltu	a0, t1, 1b	/* t1 - start of aligned dst */

.Lskip_align_dst:
	/*
	 * Now dst is aligned.
	 * Use shift-copy if src is misaligned.
	 * Use word-copy if both src and dst are aligned because
	 * can not use shift-copy which do not require shifting
	 */
	/* a1 - start of src */
	andi	a3, a1, XLENB-1
	bnez	a3, .Lshift_copy

.Lword_copy:
        /*
	 * Both src and dst are aligned, unrolled word copy
	 *
	 * a0 - start of aligned dst
	 * a1 - start of aligned src
	 * t0 - end of aligned dst
	 */
	addi	t0, t0, -(8*XLENB) /* not to over run */
2:
	fixup REG_L   a4,        0(a1), 10f
	fixup REG_L   a5,    XLENB(a1), 10f
	fixup REG_L   a6,  2*XLENB(a1), 10f
	fixup REG_L   a7,  3*XLENB(a1), 10f
	fixup REG_L   t1,  4*XLENB(a1), 10f
	fixup REG_L   t2,  5*XLENB(a1), 10f
	fixup REG_L   t3,  6*XLENB(a1), 10f
	fixup REG_L   t4,  7*XLENB(a1), 10f
	fixup REG_S   a4,        0(a0), 10f
	fixup REG_S   a5,    XLENB(a0), 10f
	fixup REG_S   a6,  2*XLENB(a0), 10f
	fixup REG_S   a7,  3*XLENB(a0), 10f
	fixup REG_S   t1,  4*XLENB(a0), 10f
	fixup REG_S   t2,  5*XLENB(a0), 10f
	fixup REG_S   t3,  6*XLENB(a0), 10f
	fixup REG_S   t4,  7*XLENB(a0), 10f
	addi	a0, a0, 8*XLENB
	addi	a1, a1, 8*XLENB
	bleu	a0, t0, 2b

	addi	t0, t0, 8*XLENB /* revert to original value */
	j	.Lbyte_copy_tail

.Lshift_copy:

	/*
	 * Word copy with shifting.
	 * For misaligned copy we still perform aligned word copy, but
	 * we need to use the value fetched from the previous iteration and
	 * do some shifts.
	 * This is safe because reading is less than a word size.
	 *
	 * a0 - start of aligned dst
	 * a1 - start of src
	 * a3 - a1 & mask:(XLENB-1)
	 * t0 - end of uncopied dst
	 * t1 - end of aligned dst
	 */
	/* calculating aligned word boundary for dst */
	andi	t1, t0, ~(XLENB-1)
	/* Converting unaligned src to aligned src */
	andi	a1, a1, ~(XLENB-1)

	/*
	 * Calculate shifts
	 * t3 - prev shift
	 * t4 - current shift
	 */
	slli	t3, a3, 3 /* converting bytes in a3 to bits */
	li	a5, XLENB*8
	sub	t4, a5, t3

	/* Load the first word to combine with second word */
	fixup REG_L   a5, 0(a1), 10f

3:
	/* Main shifting copy
	 *
	 * a0 - start of aligned dst
	 * a1 - start of aligned src
	 * t1 - end of aligned dst
	 */

	/* At least one iteration will be executed */
	srl	a4, a5, t3
	fixup REG_L   a5, XLENB(a1), 10f
	addi	a1, a1, XLENB
	sll	a2, a5, t4
	or	a2, a2, a4
	fixup REG_S   a2, 0(a0), 10f
	addi	a0, a0, XLENB
	bltu	a0, t1, 3b

	/* Revert src to original unaligned value  */
	add	a1, a1, a3

.Lbyte_copy_tail:
	/*
	 * Byte copy anything left.
	 *
	 * a0 - start of remaining dst
	 * a1 - start of remaining src
	 * t0 - end of remaining dst
	 */
	bgeu	a0, t0, .Lout_copy_user  /* check if end of copy */
4:
	fixup lb      a5, 0(a1), 10f
	addi	a1, a1, 1	/* src */
	fixup sb      a5, 0(a0), 10f
	addi	a0, a0, 1	/* dst */
	bltu	a0, t0, 4b	/* t0 - end of dst */

.Lout_copy_user:
	li	a0, 0
	ret
10:
	sub a0, t5, a0
	ret
