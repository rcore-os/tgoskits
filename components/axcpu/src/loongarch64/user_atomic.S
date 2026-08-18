.macro _asm_nofault_extable from, to
    .pushsection __nofault_ex_table, "a"
    .balign 4
    .word \from - .
    .word \to - .
    .popsection
.endm

.section .text
.global __axcpu_user_read_u32
.type __axcpu_user_read_u32, @function
__axcpu_user_read_u32:
1:
    ld.wu $t0, $a0, 0
    st.w $t0, $a1, 0
    move $a0, $zero
    jr $ra
.Luser_read_fault:
    addi.w $a0, $zero, 1
    jr $ra
    _asm_nofault_extable 1b, .Luser_read_fault
.size __axcpu_user_read_u32, . - __axcpu_user_read_u32

.global __axcpu_user_atomic_u32
.type __axcpu_user_atomic_u32, @function
__axcpu_user_atomic_u32:
    li.w $t3, 128
1:
    ll.w $t0, $a0, 0
    beqz $a1, .Luser_atomic_set
    addi.w $t2, $zero, 1
    beq $a1, $t2, .Luser_atomic_add
    addi.w $t2, $zero, 2
    beq $a1, $t2, .Luser_atomic_or
    addi.w $t2, $zero, 3
    beq $a1, $t2, .Luser_atomic_and_not
    xor $t1, $t0, $a2
    b 2f
.Luser_atomic_set:
    move $t1, $a2
    b 2f
.Luser_atomic_add:
    add.w $t1, $t0, $a2
    b 2f
.Luser_atomic_or:
    or $t1, $t0, $a2
    b 2f
.Luser_atomic_and_not:
    nor $t2, $a2, $zero
    and $t1, $t0, $t2
2:
    sc.w $t1, $a0, 0
    bnez $t1, .Luser_atomic_success
    addi.w $t3, $t3, -1
    bnez $t3, 1b
    dbar 0
    addi.w $a0, $zero, 2
    jr $ra
.Luser_atomic_success:
    dbar 0
    st.w $t0, $a3, 0
    move $a0, $zero
    jr $ra
.Luser_atomic_fault:
    addi.w $a0, $zero, 1
    jr $ra
    _asm_nofault_extable 1b, .Luser_atomic_fault
    _asm_nofault_extable 2b, .Luser_atomic_fault
.size __axcpu_user_atomic_u32, . - __axcpu_user_atomic_u32
