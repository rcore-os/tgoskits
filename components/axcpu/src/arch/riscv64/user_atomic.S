.macro _asm_nofault_extable from, to
    .pushsection __nofault_ex_table, "a"
    .balign 4
    .word \from - _nofault_ex_table_start
    .word \to - _nofault_ex_table_start
    .popsection
.endm

.section .text
.global __axcpu_user_read_u32
.type __axcpu_user_read_u32, @function
__axcpu_user_read_u32:
1:
    lw t0, 0(a0)
    sw t0, 0(a1)
    li a0, 0
    ret

.Luser_read_fault:
    li a0, 1
    ret

    _asm_nofault_extable 1b, .Luser_read_fault
.size __axcpu_user_read_u32, . - __axcpu_user_read_u32

/*
 * LLVM 22 validates the ratified A extension as the split Zaamo/Zalrsc
 * features, while Rust's riscv64gc target still advertises the aggregate
 * `+a` feature. This routine uses only AMO instructions, so declare the
 * precise subset locally without changing the platform-wide ISA contract.
 */
.option push
.option arch, +zaamo

.global __axcpu_user_atomic_u32
.type __axcpu_user_atomic_u32, @function
__axcpu_user_atomic_u32:
    beqz a1, .Luser_atomic_set
    li t1, 1
    beq a1, t1, .Luser_atomic_add
    li t1, 2
    beq a1, t1, .Luser_atomic_or
    li t1, 3
    beq a1, t1, .Luser_atomic_and_not

.Luser_atomic_xor:
1:  amoxor.w.aqrl t0, a2, (a0)
    j .Luser_atomic_success

.Luser_atomic_set:
2:  amoswap.w.aqrl t0, a2, (a0)
    j .Luser_atomic_success

.Luser_atomic_add:
3:  amoadd.w.aqrl t0, a2, (a0)
    j .Luser_atomic_success

.Luser_atomic_or:
4:  amoor.w.aqrl t0, a2, (a0)
    j .Luser_atomic_success

.Luser_atomic_and_not:
    not t1, a2
5:  amoand.w.aqrl t0, t1, (a0)

.Luser_atomic_success:
    sw t0, 0(a3)
    li a0, 0
    ret

.Luser_atomic_fault:
    li a0, 1
    ret

    _asm_nofault_extable 1b, .Luser_atomic_fault
    _asm_nofault_extable 2b, .Luser_atomic_fault
    _asm_nofault_extable 3b, .Luser_atomic_fault
    _asm_nofault_extable 4b, .Luser_atomic_fault
    _asm_nofault_extable 5b, .Luser_atomic_fault
.size __axcpu_user_atomic_u32, . - __axcpu_user_atomic_u32

.option pop

.option push
.option arch, +zalrsc
.global __axcpu_user_cmpxchg_u32
.type __axcpu_user_cmpxchg_u32, @function
__axcpu_user_cmpxchg_u32:
    // LR.W sign-extends; compare the same representation when bit 31 is set.
    .if XLENB == 8
    sext.w a1, a1
    .endif
    li t2, 128
1:
    lr.w t0, (a0)
    bne t0, a1, .Luser_cmpxchg_observed
2:
    sc.w.aqrl t1, a2, (a0)
    beqz t1, .Luser_cmpxchg_observed
    addi t2, t2, -1
    bnez t2, 1b
    li a0, 2
    ret
.Luser_cmpxchg_observed:
    sw t0, 0(a3)
    li a0, 0
    ret
.Luser_cmpxchg_fault:
    li a0, 1
    ret
    _asm_nofault_extable 1b, .Luser_cmpxchg_fault
    _asm_nofault_extable 2b, .Luser_cmpxchg_fault
.size __axcpu_user_cmpxchg_u32, . - __axcpu_user_cmpxchg_u32
.option pop
