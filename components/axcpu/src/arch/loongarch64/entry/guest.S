.section .text.ax_cpu_lvz_guest, "ax"

.equ CSR_GSTAT,        0x50
.equ CSR_GTLBC,        0x15
.equ CSR_CRMD,         0x0
.equ CSR_PRMD,         0x1
.equ CSR_ECFG,         0x4
.equ CSR_EENTRY,       0xc
.equ CSR_ASID,         0x18
.equ CSR_PGDL,         0x19
.equ CSR_PGDH,         0x1a
.equ CSR_BADI,         0x8
// Host KS ownership is shared with normal LoongArch trap entry. KS0-KS2
// belong to exception handling, KS3 is the immutable CPU-local shadow, and
// KS4-KS5 belong to virtualization.
.equ HOST_TRAP_KSP_KS, 0x30
.equ HOST_TRAP_T0_KS,  0x31
.equ HOST_TRAP_T1_KS,  0x32
.equ HOST_PERCPU_KS,  0x33
.equ HOST_VCPU_KS,   0x34
.equ HOST_VCPU_TMP_KS, 0x35
.equ CSR_PGD,          0x1b
.equ CSR_PWCL,         0x1c
.equ CSR_PWCH,         0x1d
.equ CSR_STLBPS,       0x1e
.equ CSR_TLBRERA,      0x8a
.equ CSR_TLBRENTRY,    0x88
.equ CSR_TLBRSAVE,     0x8b
.equ CSR_TLBRELO0,     0x8c
.equ CSR_TLBRELO1,     0x8d
.equ CSR_TLBREHI,      0x8e
.equ CSR_TLBRPRMD,     0x8f
.equ CSR_GSTAT_PVM,    0x2
.equ CSR_TLBRPRMD_PGM, 0x8
.equ CSR_GTLBC_TGID,   0xff0000
.equ CSR_TIMER_IE,     0x800
.equ CSR_CRMD_IE,      0x4
.equ STAGE2_PWCL,      0x13e4d52c
.equ STAGE2_PWCH,      0x267
.equ STAGE2_STLBPS,    12
.equ STAGE2_ASID,      1
.equ ECFG_VS_MASK,     0x70000
.equ ECFG_VS_5,        0x50000

.macro SWITCH_TO_GUEST_TRANSLATION
    ld.d    $t0, $sp, {guest_tlbrentry}
    csrwr   $t0, CSR_TLBRENTRY
    ld.d    $t0, $sp, {guest_eentry}
    csrwr   $t0, CSR_EENTRY
    csrrd   $t1, CSR_ECFG
    li.d    $t0, ECFG_VS_MASK
    andn    $t1, $t1, $t0
    li.d    $t0, ECFG_VS_5
    or      $t1, $t1, $t0
    csrwr   $t1, CSR_ECFG

    li.d    $t0, STAGE2_PWCL
    csrwr   $t0, CSR_PWCL
    li.d    $t0, STAGE2_PWCH
    csrwr   $t0, CSR_PWCH
    li.d    $t0, STAGE2_STLBPS
    csrwr   $t0, CSR_STLBPS
    csrwr   $a1, CSR_PGDL
    csrwr   $a1, CSR_PGDH
    li.d    $t0, STAGE2_ASID
    csrwr   $t0, CSR_ASID
    invtlb  0x0, $r0, $r0
.endm

.macro RESTORE_HOST_TRANSLATION
    ld.d    $t0, $sp, {host_eentry}
    csrwr   $t0, CSR_EENTRY
    ld.d    $t0, $sp, {host_ecfg}
    csrwr   $t0, CSR_ECFG
    ld.d    $t0, $sp, {host_tlbrentry}
    csrwr   $t0, CSR_TLBRENTRY
    ld.d    $t0, $sp, {host_asid}
    csrwr   $t0, CSR_ASID
    ld.d    $t0, $sp, {host_pwcl}
    csrwr   $t0, CSR_PWCL
    ld.d    $t0, $sp, {host_pwch}
    csrwr   $t0, CSR_PWCH
    ld.d    $t0, $sp, {host_stlbps}
    csrwr   $t0, CSR_STLBPS
    ld.d    $t0, $sp, {host_pgdl}
    csrwr   $t0, CSR_PGDL
    ld.d    $t0, $sp, {host_pgdh}
    csrwr   $t0, CSR_PGDH
    invtlb  0x0, $r0, $r0
.endm

.macro SAVE_GUEST_REGS
    st.d    $r0,  $sp, {gpr0}
    st.d    $r1,  $sp, {gpr1}
    st.d    $r2,  $sp, {gpr2}
    csrrd   $t0, 0x502
    st.d    $t0, $sp, {gpr3}
    st.d    $r4,  $sp, {gpr4}
    st.d    $r5,  $sp, {gpr5}
    st.d    $r6,  $sp, {gpr6}
    st.d    $r7,  $sp, {gpr7}
    st.d    $r8,  $sp, {gpr8}
    st.d    $r9,  $sp, {gpr9}
    st.d    $r10, $sp, {gpr10}
    st.d    $r11, $sp, {gpr11}
    csrrd   $t0, HOST_VCPU_TMP_KS
    st.d    $t0, $sp, {gpr12}
    st.d    $r13, $sp, {gpr13}
    st.d    $r14, $sp, {gpr14}
    st.d    $r15, $sp, {gpr15}
    st.d    $r16, $sp, {gpr16}
    st.d    $r17, $sp, {gpr17}
    st.d    $r18, $sp, {gpr18}
    st.d    $r19, $sp, {gpr19}
    st.d    $r20, $sp, {gpr20}
    st.d    $r21, $sp, {gpr21}
    // Guest r21 is durable. Restore the physical CPU's area base before any
    // later exit work can enter host Rust.
    RESTORE_HOST_PERCPU
    st.d    $r22, $sp, {gpr22}
    st.d    $r23, $sp, {gpr23}
    st.d    $r24, $sp, {gpr24}
    st.d    $r25, $sp, {gpr25}
    st.d    $r26, $sp, {gpr26}
    st.d    $r27, $sp, {gpr27}
    st.d    $r28, $sp, {gpr28}
    st.d    $r29, $sp, {gpr29}
    st.d    $r30, $sp, {gpr30}
    st.d    $r31, $sp, {gpr31}

    csrrd   $t0, 0x6
    st.d    $t0, $sp, {sepc}
    gcsrrd  $t0, 0x0
    st.d    $t0, $sp, {gcsr_crmd}
    gcsrrd  $t0, 0x1
    st.d    $t0, $sp, {gcsr_prmd}
    gcsrrd  $t0, 0x2
    st.d    $t0, $sp, {gcsr_euen}
    gcsrrd  $t0, 0x3
    st.d    $t0, $sp, {gcsr_misc}
    gcsrrd  $t0, 0x4
    st.d    $t0, $sp, {gcsr_ectl}
    gcsrrd  $t0, 0x5
    st.d    $t0, $sp, {gcsr_estat}
    gcsrrd  $t0, 0x6
    st.d    $t0, $sp, {gcsr_era}
    gcsrrd  $t0, 0x7
    st.d    $t0, $sp, {gcsr_badv}
    gcsrrd  $t0, 0x8
    st.d    $t0, $sp, {gcsr_badi}
    gcsrrd  $t0, 0xc
    st.d    $t0, $sp, {gcsr_eentry}
    gcsrrd  $t0, 0x10
    st.d    $t0, $sp, {gcsr_tlbidx}
    gcsrrd  $t0, 0x11
    st.d    $t0, $sp, {gcsr_tlbehi}
    gcsrrd  $t0, 0x12
    st.d    $t0, $sp, {gcsr_tlbelo0}
    gcsrrd  $t0, 0x13
    st.d    $t0, $sp, {gcsr_tlbelo1}
    gcsrrd  $t0, 0x18
    st.d    $t0, $sp, {gcsr_asid}
    gcsrrd  $t0, 0x19
    st.d    $t0, $sp, {gcsr_pgdl}
    gcsrrd  $t0, 0x1a
    st.d    $t0, $sp, {gcsr_pgdh}
    gcsrrd  $t0, 0x1b
    st.d    $t0, $sp, {gcsr_pgd}
    gcsrrd  $t0, 0x1c
    st.d    $t0, $sp, {gcsr_pwcl}
    gcsrrd  $t0, 0x1d
    st.d    $t0, $sp, {gcsr_pwch}
    gcsrrd  $t0, 0x1e
    st.d    $t0, $sp, {gcsr_stlbps}
    gcsrrd  $t0, 0x1f
    st.d    $t0, $sp, {gcsr_ravcfg}
    gcsrrd  $t0, 0x20
    st.d    $t0, $sp, {gcsr_cpuid}
    gcsrrd  $t0, 0x21
    st.d    $t0, $sp, {gcsr_prcfg1}
    gcsrrd  $t0, 0x22
    st.d    $t0, $sp, {gcsr_prcfg2}
    gcsrrd  $t0, 0x23
    st.d    $t0, $sp, {gcsr_prcfg3}
    gcsrrd  $t0, 0x30
    st.d    $t0, $sp, {gcsr_save0}
    gcsrrd  $t0, 0x31
    st.d    $t0, $sp, {gcsr_save1}
    gcsrrd  $t0, 0x32
    st.d    $t0, $sp, {gcsr_save2}
    gcsrrd  $t0, 0x33
    st.d    $t0, $sp, {gcsr_save3}
    gcsrrd  $t0, 0x34
    st.d    $t0, $sp, {gcsr_save4}
    gcsrrd  $t0, 0x35
    st.d    $t0, $sp, {gcsr_save5}
    gcsrrd  $t0, 0x36
    st.d    $t0, $sp, {gcsr_save6}
    gcsrrd  $t0, 0x37
    st.d    $t0, $sp, {gcsr_save7}
    gcsrrd  $t0, 0x38
    st.d    $t0, $sp, {gcsr_save8}
    gcsrrd  $t0, 0x39
    st.d    $t0, $sp, {gcsr_save9}
    gcsrrd  $t0, 0x3a
    st.d    $t0, $sp, {gcsr_save10}
    gcsrrd  $t0, 0x3b
    st.d    $t0, $sp, {gcsr_save11}
    gcsrrd  $t0, 0x3c
    st.d    $t0, $sp, {gcsr_save12}
    gcsrrd  $t0, 0x3d
    st.d    $t0, $sp, {gcsr_save13}
    gcsrrd  $t0, 0x3e
    st.d    $t0, $sp, {gcsr_save14}
    gcsrrd  $t0, 0x3f
    st.d    $t0, $sp, {gcsr_save15}
    gcsrrd  $t0, 0x40
    st.d    $t0, $sp, {gcsr_tid}
    gcsrrd  $t0, 0x41
    st.d    $t0, $sp, {gcsr_tcfg}
    /*
     * Keep the software timer's logical TVAL in the Rust context. QEMU-LVZ
     * reuses GCSR_TVAL as a pause/resume remaining-ticks value while the guest
     * is stopped, so snapshotting it here pollutes the emulated guest timer.
     */
    gcsrrd  $t0, 0x43
    st.d    $t0, $sp, {gcsr_cntc}
    gcsrrd  $t0, 0x44
    st.d    $t0, $sp, {gcsr_ticlr}
    gcsrrd  $t0, 0x60
    st.d    $t0, $sp, {gcsr_llbctl}
    gcsrrd  $t0, 0x88
    st.d    $t0, $sp, {gcsr_tlbrentry}
    gcsrrd  $t0, 0x89
    st.d    $t0, $sp, {gcsr_tlbrbadv}
    gcsrrd  $t0, 0x8a
    st.d    $t0, $sp, {gcsr_tlbrera}
    gcsrrd  $t0, 0x8b
    st.d    $t0, $sp, {gcsr_tlbrsave}
    gcsrrd  $t0, 0x8c
    st.d    $t0, $sp, {gcsr_tlbrelo0}
    gcsrrd  $t0, 0x8d
    st.d    $t0, $sp, {gcsr_tlbrelo1}
    gcsrrd  $t0, 0x8e
    st.d    $t0, $sp, {gcsr_tlbrehi}
    gcsrrd  $t0, 0x8f
    st.d    $t0, $sp, {gcsr_tlbrprmd}
    gcsrrd  $t0, 0x180
    st.d    $t0, $sp, {gcsr_dmw0}
    gcsrrd  $t0, 0x181
    st.d    $t0, $sp, {gcsr_dmw1}
    gcsrrd  $t0, 0x182
    st.d    $t0, $sp, {gcsr_dmw2}
    gcsrrd  $t0, 0x183
    st.d    $t0, $sp, {gcsr_dmw3}

    csrrd   $t0, 0x5
    st.d    $t0, $sp, {host_estat}
    csrrd   $t0, 0x6
    st.d    $t0, $sp, {host_era}
    csrrd   $t0, 0x7
    st.d    $t0, $sp, {host_badv}
    csrrd   $t0, CSR_BADI
    st.d    $t0, $sp, {host_badi}
    csrrd   $t0, 0x89
    st.d    $t0, $sp, {host_tlbrbadv}
    csrrd   $t0, CSR_TLBRERA
    st.d    $t0, $sp, {host_tlbrera}
.endm

.macro RESTORE_HOST_PERCPU
    csrrd   $r21, HOST_PERCPU_KS
.endm

.macro ENTER_HOST_MODE
    li.d    $t0, CSR_GSTAT_PVM
    csrxchg $zero, $t0, CSR_GSTAT
    csrrd   $t1, CSR_TLBRPRMD
    li.d    $t0, -9
    and     $t1, $t1, $t0
    csrwr   $t1, CSR_TLBRPRMD
    csrrd   $t1, CSR_GTLBC
    li.d    $t0, CSR_GTLBC_TGID
    andn    $t1, $t1, $t0
    csrwr   $t1, CSR_GTLBC
.endm

.macro DISABLE_TIMER_IRQ_WITH_SAVED_GUEST_T0_T1
    csrwr   $t1, HOST_TRAP_T1_KS
    csrrd   $t0, CSR_ECFG
    li.d    $t1, CSR_TIMER_IE
    andn    $t0, $t0, $t1
    csrwr   $t0, CSR_ECFG
    csrrd   $t1, HOST_TRAP_T1_KS
.endm

.macro PREPARE_GUEST_ENTRY
    li.d    $t0, CSR_CRMD_IE
    csrxchg $zero, $t0, CSR_CRMD

    csrwr   $zero, CSR_PRMD

    csrrd   $t1, CSR_TLBRERA
    li.d    $t0, -2
    and     $t1, $t1, $t0
    csrwr   $t1, CSR_TLBRERA

    csrrd   $t0, CSR_ECFG
    li.d    $t1, CSR_TIMER_IE
    andn    $t0, $t0, $t1
    csrwr   $t0, CSR_ECFG
.endm

.macro RESTORE_GUEST_REGS
    ld.d    $t0, $sp, {gpr3}
    csrwr   $t0, 0x502

    ld.d    $t0, $sp, {sepc}
    csrwr   $t0, 0x6
    ld.d    $t0, $sp, {gcsr_crmd}
    gcsrwr  $t0, 0x0
    ld.d    $t0, $sp, {gcsr_prmd}
    gcsrwr  $t0, 0x1
    ld.d    $t0, $sp, {gcsr_euen}
    gcsrwr  $t0, 0x2
    ld.d    $t0, $sp, {gcsr_misc}
    gcsrwr  $t0, 0x3
    ld.d    $t0, $sp, {gcsr_ectl}
    gcsrwr  $t0, 0x4
    gcsrrd  $t1, 0x5
    li.d    $t2, 0x0fff
    and     $t1, $t1, $t2
    ld.d    $t0, $sp, {gcsr_estat}
    or      $t0, $t0, $t1
    gcsrwr  $t0, 0x5
    ld.d    $t0, $sp, {gcsr_era}
    gcsrwr  $t0, 0x6
    ld.d    $t0, $sp, {gcsr_badv}
    gcsrwr  $t0, 0x7
    ld.d    $t0, $sp, {gcsr_badi}
    gcsrwr  $t0, 0x8
    ld.d    $t0, $sp, {gcsr_eentry}
    gcsrwr  $t0, 0xc
    ld.d    $t0, $sp, {gcsr_tlbidx}
    gcsrwr  $t0, 0x10
    ld.d    $t0, $sp, {gcsr_tlbehi}
    gcsrwr  $t0, 0x11
    ld.d    $t0, $sp, {gcsr_tlbelo0}
    gcsrwr  $t0, 0x12
    ld.d    $t0, $sp, {gcsr_tlbelo1}
    gcsrwr  $t0, 0x13
    ld.d    $t0, $sp, {gcsr_asid}
    gcsrwr  $t0, 0x18
    ld.d    $t0, $sp, {gcsr_pgdl}
    gcsrwr  $t0, 0x19
    ld.d    $t0, $sp, {gcsr_pgdh}
    gcsrwr  $t0, 0x1a
    ld.d    $t0, $sp, {gcsr_pgd}
    gcsrwr  $t0, 0x1b
    ld.d    $t0, $sp, {gcsr_pwcl}
    gcsrwr  $t0, 0x1c
    ld.d    $t0, $sp, {gcsr_pwch}
    gcsrwr  $t0, 0x1d
    ld.d    $t0, $sp, {gcsr_stlbps}
    gcsrwr  $t0, 0x1e
    ld.d    $t0, $sp, {gcsr_ravcfg}
    gcsrwr  $t0, 0x1f
    ld.d    $t0, $sp, {gcsr_cpuid}
    gcsrwr  $t0, 0x20
    ld.d    $t0, $sp, {gcsr_prcfg1}
    gcsrwr  $t0, 0x21
    ld.d    $t0, $sp, {gcsr_prcfg2}
    gcsrwr  $t0, 0x22
    ld.d    $t0, $sp, {gcsr_prcfg3}
    gcsrwr  $t0, 0x23
    ld.d    $t0, $sp, {gcsr_save0}
    gcsrwr  $t0, 0x30
    ld.d    $t0, $sp, {gcsr_save1}
    gcsrwr  $t0, 0x31
    ld.d    $t0, $sp, {gcsr_save2}
    gcsrwr  $t0, 0x32
    ld.d    $t0, $sp, {gcsr_save3}
    gcsrwr  $t0, 0x33
    ld.d    $t0, $sp, {gcsr_save4}
    gcsrwr  $t0, 0x34
    ld.d    $t0, $sp, {gcsr_save5}
    gcsrwr  $t0, 0x35
    ld.d    $t0, $sp, {gcsr_save6}
    gcsrwr  $t0, 0x36
    ld.d    $t0, $sp, {gcsr_save7}
    gcsrwr  $t0, 0x37
    ld.d    $t0, $sp, {gcsr_save8}
    gcsrwr  $t0, 0x38
    ld.d    $t0, $sp, {gcsr_save9}
    gcsrwr  $t0, 0x39
    ld.d    $t0, $sp, {gcsr_save10}
    gcsrwr  $t0, 0x3a
    ld.d    $t0, $sp, {gcsr_save11}
    gcsrwr  $t0, 0x3b
    ld.d    $t0, $sp, {gcsr_save12}
    gcsrwr  $t0, 0x3c
    ld.d    $t0, $sp, {gcsr_save13}
    gcsrwr  $t0, 0x3d
    ld.d    $t0, $sp, {gcsr_save14}
    gcsrwr  $t0, 0x3e
    ld.d    $t0, $sp, {gcsr_save15}
    gcsrwr  $t0, 0x3f
    ld.d    $t0, $sp, {gcsr_tid}
    gcsrwr  $t0, 0x40
    ld.d    $t0, $sp, {gcsr_tcfg}
    gcsrwr  $t0, 0x41
    /*
     * QEMU-LVZ stores GCSR_TVAL as remaining ticks while guest mode is
     * paused.  Rewriting it on every VM entry re-bases the relative guest
     * timer and can keep pushing one-shot deadlines into the future.
     */
    ld.d    $t0, $sp, {gcsr_cntc}
    gcsrwr  $t0, 0x43
    ld.d    $t0, $sp, {gcsr_ticlr}
    gcsrwr  $t0, 0x44
    ld.d    $t0, $sp, {gcsr_llbctl}
    gcsrwr  $t0, 0x60
    ld.d    $t0, $sp, {gcsr_tlbrentry}
    gcsrwr  $t0, 0x88
    ld.d    $t0, $sp, {gcsr_tlbrbadv}
    gcsrwr  $t0, 0x89
    ld.d    $t0, $sp, {gcsr_tlbrera}
    gcsrwr  $t0, 0x8a
    ld.d    $t0, $sp, {gcsr_tlbrsave}
    gcsrwr  $t0, 0x8b
    ld.d    $t0, $sp, {gcsr_tlbrelo0}
    gcsrwr  $t0, 0x8c
    ld.d    $t0, $sp, {gcsr_tlbrelo1}
    gcsrwr  $t0, 0x8d
    ld.d    $t0, $sp, {gcsr_tlbrehi}
    gcsrwr  $t0, 0x8e
    ld.d    $t0, $sp, {gcsr_tlbrprmd}
    gcsrwr  $t0, 0x8f
    ld.d    $t0, $sp, {gcsr_dmw0}
    gcsrwr  $t0, 0x180
    ld.d    $t0, $sp, {gcsr_dmw1}
    gcsrwr  $t0, 0x181
    ld.d    $t0, $sp, {gcsr_dmw2}
    gcsrwr  $t0, 0x182
    ld.d    $t0, $sp, {gcsr_dmw3}
    gcsrwr  $t0, 0x183
    ld.d    $t0, $sp, {sepc}
    csrwr   $t0, 0x6
    csrrd   $t1, CSR_TLBRERA
    li.d    $t0, -2
    and     $t1, $t1, $t0
    csrwr   $t1, CSR_TLBRERA
    csrrd   $t1, CSR_TLBRPRMD
    li.d    $t0, -9
    and     $t1, $t1, $t0
    csrwr   $t1, CSR_TLBRPRMD

    li.d    $t0, CSR_GSTAT_PVM
    csrxchg $t0, $t0, CSR_GSTAT

    ld.d    $r1,  $sp, {gpr1}
    ld.d    $r2,  $sp, {gpr2}
    ld.d    $r4,  $sp, {gpr4}
    ld.d    $r5,  $sp, {gpr5}
    ld.d    $r6,  $sp, {gpr6}
    ld.d    $r7,  $sp, {gpr7}
    ld.d    $r8,  $sp, {gpr8}
    ld.d    $r9,  $sp, {gpr9}
    ld.d    $r10, $sp, {gpr10}
    ld.d    $r11, $sp, {gpr11}
    ld.d    $r12, $sp, {gpr12}
    ld.d    $r13, $sp, {gpr13}
    ld.d    $r14, $sp, {gpr14}
    ld.d    $r15, $sp, {gpr15}
    ld.d    $r16, $sp, {gpr16}
    ld.d    $r17, $sp, {gpr17}
    ld.d    $r18, $sp, {gpr18}
    ld.d    $r19, $sp, {gpr19}
    ld.d    $r20, $sp, {gpr20}
    ld.d    $r21, $sp, {gpr21}
    ld.d    $r22, $sp, {gpr22}
    ld.d    $r23, $sp, {gpr23}
    ld.d    $r24, $sp, {gpr24}
    ld.d    $r25, $sp, {gpr25}
    ld.d    $r26, $sp, {gpr26}
    ld.d    $r27, $sp, {gpr27}
    ld.d    $r28, $sp, {gpr28}
    ld.d    $r29, $sp, {gpr29}
    ld.d    $r30, $sp, {gpr30}
    ld.d    $r31, $sp, {gpr31}
    ld.d    $r0,  $sp, {gpr0}
    csrrd   $sp, 0x502
.endm

.global __ax_cpu_lvz_run_guest
.hidden __ax_cpu_lvz_run_guest
__ax_cpu_lvz_run_guest:
    move    $t1, $ra
    addi.d  $t0, $a0, {ctx_size}
    csrwr   $t0, HOST_VCPU_KS
    move    $sp, $a0
    SWITCH_TO_GUEST_TRANSLATION
    PREPARE_GUEST_ENTRY
    RESTORE_GUEST_REGS
    ertn
    jr      $t1

.global __ax_cpu_lvz_guest_exit
.hidden __ax_cpu_lvz_guest_exit
__ax_cpu_lvz_guest_exit:
    b       __ax_cpu_lvzvmexit_trampoline

.balign 4096
.global __ax_cpu_lvz_guest_tlb_refill_vector
.hidden __ax_cpu_lvz_guest_tlb_refill_vector
__ax_cpu_lvz_guest_tlb_refill_vector:
    csrwr   $t0, CSR_TLBRSAVE
    csrrd   $t0, CSR_PGD

    AX_CPU_WALK_4K .Lguest_stage2_tlb_invalid
    tlbfill
    csrrd   $t0, CSR_TLBRSAVE
    ertn

.Lguest_stage2_tlb_invalid:
    csrrd   $t0, CSR_TLBRSAVE
    csrwr   $t0, HOST_VCPU_TMP_KS
    DISABLE_TIMER_IRQ_WITH_SAVED_GUEST_T0_T1
    csrwr   $sp, 0x502
    csrrd   $sp, HOST_VCPU_KS
    addi.d  $sp, $sp, -{ctx_size}
    SAVE_GUEST_REGS
    ENTER_HOST_MODE
    RESTORE_HOST_TRANSLATION
    li.d    $a0, 0
    b       __ax_cpu_lvz_guest_exit

.section .text.ax_cpu_lvz_vectors, "ax"
.align 12
.global __ax_cpu_lvz_exception_vectors
.hidden __ax_cpu_lvz_exception_vectors
__ax_cpu_lvz_exception_vectors:
    .org 0x0
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x80
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x100
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x180
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x200
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x280
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x400
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x480
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x500
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x580
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x600
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x680
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x700
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x780
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x800
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x880
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x900
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0x980
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0xa00
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0xa80
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0xb00
    b       __ax_cpu_lvz_hvc_handler_vector
    .org 0xb80
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0xc00
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0xc80
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0xd00
    b       __ax_cpu_lvz_sync_handler_vector
    .org 0xd80
    b       __ax_cpu_lvz_sync_handler_vector

    /*
     * Hardware interrupts use EENTRY + (64 + vector) * vec_size.  With VS=5
     * each slot is 0x80 bytes, so local interrupt 11 (timer) lands at 0x2580.
     */
    .org 0x2000
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2080
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2100
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2180
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2200
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2280
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2300
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2380
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2400
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2480
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2500
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2580
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2600
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2680
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2700
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2780
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2800
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2880
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2900
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2980
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2a00
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2a80
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2b00
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2b80
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2c00
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2c80
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2d00
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2d80
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2e00
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2e80
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2f00
    b       __ax_cpu_lvz_irq_handler_vector
    .org 0x2f80
    b       __ax_cpu_lvz_irq_handler_vector

__ax_cpu_lvz_irq_handler_vector:
    csrwr   $t0, HOST_VCPU_TMP_KS
    DISABLE_TIMER_IRQ_WITH_SAVED_GUEST_T0_T1
    csrwr   $sp, 0x502
    csrrd   $sp, HOST_VCPU_KS
    addi.d  $sp, $sp, -{ctx_size}
    SAVE_GUEST_REGS
    ENTER_HOST_MODE
    RESTORE_HOST_TRANSLATION
    li.d    $a0, 1
    b       __ax_cpu_lvz_guest_exit

__ax_cpu_lvz_sync_handler_vector:
    csrwr   $t0, HOST_VCPU_TMP_KS
    DISABLE_TIMER_IRQ_WITH_SAVED_GUEST_T0_T1
    csrwr   $sp, 0x502
    csrrd   $sp, HOST_VCPU_KS
    addi.d  $sp, $sp, -{ctx_size}
    SAVE_GUEST_REGS
    ENTER_HOST_MODE
    RESTORE_HOST_TRANSLATION
    li.d    $a0, 0
    b       __ax_cpu_lvz_guest_exit

__ax_cpu_lvz_hvc_handler_vector:
    csrwr   $t0, HOST_VCPU_TMP_KS
    DISABLE_TIMER_IRQ_WITH_SAVED_GUEST_T0_T1
    csrwr   $sp, 0x502
    csrrd   $sp, HOST_VCPU_KS
    addi.d  $sp, $sp, -{ctx_size}
    SAVE_GUEST_REGS
    ENTER_HOST_MODE
    RESTORE_HOST_TRANSLATION
    li.d    $a0, 0
    b       __ax_cpu_lvz_guest_exit
