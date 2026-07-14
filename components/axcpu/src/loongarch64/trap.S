.section .text
.balign 4096
.global exception_entry_base
exception_entry_base:
    csrwr   $t0, KSAVE_T0
    csrwr   $t1, KSAVE_T1

    csrrd   $t1, LA_CSR_PRMD
    andi    $t1, $t1, 0x3
    move    $t0, $sp
    bnez    $t1, .Ltrap_from_user

    csrwr   $t0, KSAVE_KSP
    addi.d  $sp, $sp, -{trapframe_size}
    b       .Ltrap_stack_ready

.Ltrap_from_user:
    csrrd   $sp, KSAVE_KSP
    csrwr   $t0, KSAVE_KSP

.Ltrap_stack_ready:
    csrrd   $t0, KSAVE_T0
    csrrd   $t1, KSAVE_T1
    PUSH_GENERAL_REGS

    csrrd   $t0, KSAVE_KSP
    csrwr   $r0, KSAVE_KSP
    csrrd   $t1, LA_CSR_PRMD
    csrrd   $t2, LA_CSR_ERA
    STD     $t0, $sp, 3
    STD     $t1, $sp, 32    // prmd
    STD     $t2, $sp, 33    // era

    // On a user trap the saved r21 is the user's `u0`. Recover the immutable
    // kernel per-CPU base only after PUSH_GENERAL_REGS captured that value.
    // A kernel trap already has the correct live r21 and must leave it alone.
    andi    $t2, $t1, 0x3
    beqz    $t2, .Lkernel_percpu_ready
    csrrd   $r21, KSAVE_PERCPU
.Lkernel_percpu_ready:
    move    $a0, $sp

    bnez    $t2, .Lexit_user

    la.pcrel $ra, .Ltrap_return
    b       loongarch64_trap_handler

.Lexit_user:
    LDD     $sp, $a0, 0
    STD     $r0, $a0, 0
    LDD     $ra, $sp, 0
    LDD     $tp, $sp, 1
    LDD     $fp, $sp, 2
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
    andi    $t2, $t0, 0x3   // saved prmd.PPLV != 0 means user mode
    csrwr   $t0, LA_CSR_PRMD
    csrwr   $t1, LA_CSR_ERA

    // A kernel trap may resume on another CPU after IRQ-return scheduling.
    // Keep that CPU's live per-CPU base; only a user return restores user u0.
    bnez    $t2, .Lrestore_user_regs
    RESTORE_KERNEL_GENERAL_REGS
    b       .Lrestore_sp

.Lrestore_user_regs:
    RESTORE_USER_GENERAL_REGS

.Lrestore_sp:
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
