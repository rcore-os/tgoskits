.section .text
.balign 4096
.global exception_entry_base
exception_entry_base:
    csrwr   $sp, KSAVE_KSP
    bnez    $sp, .Ltrap_entry

    csrrd   $sp, KSAVE_KSP
    addi.d  $sp, $sp, -{trapframe_size}

.Ltrap_entry:
    PUSH_GENERAL_REGS

    csrrd   $t0, KSAVE_KSP
    csrwr   $r0, KSAVE_KSP
    csrrd   $t1, LA_CSR_PRMD
    csrrd   $t2, LA_CSR_ERA
    STD     $t0, $sp, 3
    STD     $t1, $sp, 32    // prmd
    STD     $t2, $sp, 33    // era

    andi    $t1, $t1, 0x3
    bnez    $t1, .Lexit_user

    move    $a0, $sp
    la.abs  $ra, .Ltrap_return
    b       loongarch64_trap_handler

.Lexit_user:
    LDD     $sp, $sp, 0
    LDD     $ra, $sp, 0
    LDD     $tp, $sp, 1
    LDD     $fp, $sp, 2
    LDD     $r21,$sp, 3
    LDD     $s0, $sp, 4
    LDD     $s1, $sp, 5
    LDD     $s2, $sp, 6
    LDD     $s3, $sp, 7
    LDD     $s4, $sp, 8
    LDD     $s5, $sp, 9
    LDD     $s6, $sp, 10
    LDD     $s7, $sp, 11
    LDD     $s8, $sp, 12

    addi.d  $sp, $sp, 14 * 8
    ret

.global enter_user
enter_user:
    addi.d  $sp, $sp, -14 * 8
    STD     $ra, $sp, 0
    STD     $tp, $sp, 1
    STD     $fp, $sp, 2
    STD     $r21,$sp, 3
    STD     $s0, $sp, 4
    STD     $s1, $sp, 5
    STD     $s2, $sp, 6
    STD     $s3, $sp, 7
    STD     $s4, $sp, 8
    STD     $s5, $sp, 9
    STD     $s6, $sp, 10
    STD     $s7, $sp, 11
    STD     $s8, $sp, 12

    STD     $sp, $a0, 0
    move    $sp, $a0
    csrwr   $a0, KSAVE_KSP

.Ltrap_return:
    LDD     $t0, $sp, 32    // prmd
    LDD     $t1, $sp, 33    // era
    csrwr   $t0, LA_CSR_PRMD
    csrwr   $t1, LA_CSR_ERA

    POP_GENERAL_REGS
    LDD     $sp, $sp, 3

    ertn

.balign 4096
.global handle_tlb_refill
handle_tlb_refill:
    csrwr   $t0, LA_CSR_TLBRSAVE
    csrrd   $t0, LA_CSR_PGD

    lddir   $t0, $t0, 3
    beqz    $t0, .Ltlb_invalid

    lddir   $t0, $t0, 2
    beqz    $t0, .Ltlb_invalid

    lddir   $t0, $t0, 1
    beqz    $t0, .Ltlb_invalid

    ldpte   $t0, 0
    ldpte   $t0, 1

    b       .Ltlb_refill

.Ltlb_invalid:
    csrrd   $t0, LA_CSR_TLBREHI
    ori     $t0, $t0, 0xC   // 4 KiB
    csrwr   $t0, LA_CSR_TLBREHI

    rotri.d $t0, $t0, 61
    ori     $t0, $t0, 3     // NR NX
    rotri.d $t0, $t0, 3

    csrwr   $t0, LA_CSR_TLBRELO0
    csrrd   $t0, LA_CSR_TLBRELO0
    csrwr   $t0, LA_CSR_TLBRELO1

.Ltlb_refill:
    tlbfill
    csrrd   $t0, LA_CSR_TLBRSAVE
    ertn
