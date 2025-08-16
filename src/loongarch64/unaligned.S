// Modified from https://github.com/LoongsonLab/StarryOS-LoongArch/blob/main/modules/axhal/src/arch/loongarch64/unaligned.S

.macro fixup_ex from, to, fix
.if \fix
    .section .fixup, "ax"
\to: 
    li.w    $a0, -1
    jr  $ra
    .previous
.endif
    .section __ex_table, "a"
    .word   \from\()b, \to\()b
    .previous
.endm

.section .text

.global _unaligned_read
_unaligned_read:
    beqz    $a2, 5f

    li.w    $t1, 8
    li.w    $t2, 0

    addi.d  $t0, $a2, -1
    mul.d   $t1, $t0, $t1
    add.d   $a0, $a0, $t0

    beq     $a3, $zero, 2f
1:  ld.b    $t3, $a0, 0
    b   3f

2:  ld.bu   $t3, $a0, 0
3:  sll.d   $t3, $t3, $t1
    or      $t2, $t2, $t3
    addi.d  $t1, $t1, -8
    addi.d  $a0, $a0, -1
    addi.d  $a2, $a2, -1
    bgt     $a2, $zero, 2b
4:  st.d    $t2, $a1, 0

    move    $a0, $a2
    jr      $ra

5:  li.w    $a0, -1
    jr      $ra

    fixup_ex 1, 6, 1
    fixup_ex 2, 6, 0
    fixup_ex 4, 6, 0

.global  _unaligned_write
_unaligned_write:
    beqz    $a2, 3f

    li.w    $t0, 0
1:  srl.d   $t1, $a1, $t0
2:  st.b    $t1, $a0, 0
    addi.d  $t0, $t0, 8
    addi.d  $a2, $a2, -1
    addi.d  $a0, $a0, 1
    bgt     $a2, $zero, 1b

    move    $a0, $a2
    jr      $ra

3:  li.w    $a0, -1
    jr      $ra

    fixup_ex 2, 4, 1
