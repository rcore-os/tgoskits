const LOONGARCH_KSAVE_CSR_BASE: usize = 0x30;

// Assembly templates require literal `.equ` values, while cpu-local owns
// the cross-crate allocation. Tie both representations at compile time.
const _: () = {
    use cpu_local::loongarch64::{
        HOST_PERCPU_KS, HOST_VCPU_KS, HOST_VCPU_TMP_KS, KSAVE_KSP, KSAVE_T0, KSAVE_T1,
    };

    assert!(LOONGARCH_KSAVE_CSR_BASE + KSAVE_KSP == 0x30);
    assert!(LOONGARCH_KSAVE_CSR_BASE + KSAVE_T0 == 0x31);
    assert!(LOONGARCH_KSAVE_CSR_BASE + KSAVE_T1 == 0x32);
    assert!(LOONGARCH_KSAVE_CSR_BASE + HOST_PERCPU_KS == 0x33);
    assert!(LOONGARCH_KSAVE_CSR_BASE + HOST_VCPU_KS == 0x34);
    assert!(LOONGARCH_KSAVE_CSR_BASE + HOST_VCPU_TMP_KS == 0x35);
};

macro_rules! include_asm_macros {
    () => {
        r#"
        .ifndef REGS_MACROS_FLAG
        .equ REGS_MACROS_FLAG, 1

        // CSR list
        .equ LA_CSR_PRMD,          0x1
        .equ LA_CSR_EUEN,          0x2
        .equ LA_CSR_ERA,           0x6
        .equ LA_CSR_PGDL,          0x19    // Page table base address when VA[47] = 0
        .equ LA_CSR_PGDH,          0x1a    // Page table base address when VA[47] = 1
        .equ LA_CSR_PGD,           0x1b    // Page table base
        .equ LA_CSR_PWCL,          0x1c
        .equ LA_CSR_PWCH,          0x1d
        .equ LA_CSR_TLBRENTRY,     0x88    // TLB refill exception entry
        .equ LA_CSR_TLBRBADV,      0x89    // TLB refill badvaddr
        .equ LA_CSR_TLBRERA,       0x8a    // TLB refill ERA
        .equ LA_CSR_TLBRSAVE,      0x8b    // KScratch for TLB refill exception
        .equ LA_CSR_TLBRELO0,      0x8c    // TLB refill entrylo0
        .equ LA_CSR_TLBRELO1,      0x8d    // TLB refill entrylo1
        .equ LA_CSR_TLBREHI,       0x8e    // TLB refill entryhi
        .equ LA_CSR_DMW0,          0x180
        .equ LA_CSR_DMW1,          0x181

        .equ KSAVE_KSP,            0x30
        .equ KSAVE_T0,             0x31
        .equ KSAVE_T1,             0x32
        // Host scratch-register ownership follows the Linux LoongArch ABI:
        // KS0-KS2 belong to exception entry, KS3 shadows the immutable
        // per-CPU base, and KS4-KS5 are reserved for virtualization.
        .equ KSAVE_PERCPU,         0x33
        .equ KSAVE_VCPU,           0x34
        .equ KSAVE_VCPU_TMP,       0x35

        .macro STD rd, rj, off
            st.d   \rd, \rj, \off*8
        .endm
        .macro LDD rd, rj, off
            ld.d   \rd, \rj, \off*8
        .endm

        .macro PUSH_POP_GENERAL_REGS, op
            \op    $ra, $sp, 1
            \op    $tp, $sp, 2
            \op    $a0, $sp, 4
            \op    $a1, $sp, 5
            \op    $a2, $sp, 6
            \op    $a3, $sp, 7
            \op    $a4, $sp, 8
            \op    $a5, $sp, 9
            \op    $a6, $sp, 10
            \op    $a7, $sp, 11
            \op    $t0, $sp, 12
            \op    $t1, $sp, 13
            \op    $t2, $sp, 14
            \op    $t3, $sp, 15
            \op    $t4, $sp, 16
            \op    $t5, $sp, 17
            \op    $t6, $sp, 18
            \op    $t7, $sp, 19
            \op    $t8, $sp, 20
            \op    $r21,$sp, 21
            \op    $fp, $sp, 22
            \op    $s0, $sp, 23
            \op    $s1, $sp, 24
            \op    $s2, $sp, 25
            \op    $s3, $sp, 26
            \op    $s4, $sp, 27
            \op    $s5, $sp, 28
            \op    $s6, $sp, 29
            \op    $s7, $sp, 30
            \op    $s8, $sp, 31
        .endm

        .macro PUSH_GENERAL_REGS
            PUSH_POP_GENERAL_REGS STD
        .endm
        .macro POP_GENERAL_REGS
            PUSH_POP_GENERAL_REGS LDD
        .endm

        // `$r21` is the kernel per-CPU base. A kernel trap frame may resume
        // after its task migrated, so the destination CPU's live value must
        // survive the restore. In contrast, a user return uses
        // RESTORE_USER_GENERAL_REGS to restore the user's `u0` value.
        .macro RESTORE_KERNEL_GENERAL_REGS
            LDD    $ra, $sp, 1
            LDD    $tp, $sp, 2
            LDD    $a0, $sp, 4
            LDD    $a1, $sp, 5
            LDD    $a2, $sp, 6
            LDD    $a3, $sp, 7
            LDD    $a4, $sp, 8
            LDD    $a5, $sp, 9
            LDD    $a6, $sp, 10
            LDD    $a7, $sp, 11
            LDD    $t0, $sp, 12
            LDD    $t1, $sp, 13
            LDD    $t2, $sp, 14
            LDD    $t3, $sp, 15
            LDD    $t4, $sp, 16
            LDD    $t5, $sp, 17
            LDD    $t6, $sp, 18
            LDD    $t7, $sp, 19
            LDD    $t8, $sp, 20
            LDD    $fp, $sp, 22
            LDD    $s0, $sp, 23
            LDD    $s1, $sp, 24
            LDD    $s2, $sp, 25
            LDD    $s3, $sp, 26
            LDD    $s4, $sp, 27
            LDD    $s5, $sp, 28
            LDD    $s6, $sp, 29
            LDD    $s7, $sp, 30
            LDD    $s8, $sp, 31
        .endm

        .macro RESTORE_USER_GENERAL_REGS
            POP_GENERAL_REGS
        .endm

        .macro _asm_extable, from, to
            .pushsection __ex_table, "a"
            .balign 4
            .word   \from - .
            .word   \to - .
            .popsection
        .endm

        .endif"#
    };
}

#[cfg(feature = "fp-simd")]
macro_rules! include_fp_asm_macros {
    () => {
        r#"
        // NOTE: The LSX/LASX vector instructions used below are accepted by the
        // LLVM integrated assembler for this target without an explicit
        // `.option arch, +lsx/+lasx` (that GNU-as directive is rejected by
        // LLVM). LSX/LASX are enabled in EUEN.SXE/ASXE at boot, so these
        // execute correctly.

        .ifndef FP_MACROS_FLAG
        .equ FP_MACROS_FLAG, 1

        .macro SAVE_FCC, base
            movcf2gr    $t0, $fcc0
            move        $t1, $t0
            movcf2gr    $t0, $fcc1
            bstrins.d   $t1, $t0, 15, 8
            movcf2gr    $t0, $fcc2
            bstrins.d   $t1, $t0, 23, 16
            movcf2gr    $t0, $fcc3
            bstrins.d   $t1, $t0, 31, 24
            movcf2gr    $t0, $fcc4
            bstrins.d   $t1, $t0, 39, 32
            movcf2gr    $t0, $fcc5
            bstrins.d   $t1, $t0, 47, 40
            movcf2gr    $t0, $fcc6
            bstrins.d   $t1, $t0, 55, 48
            movcf2gr    $t0, $fcc7
            bstrins.d   $t1, $t0, 63, 56
            st.d        $t1, \base, 0
        .endm

        .macro RESTORE_FCC, base
            ld.d        $t0, \base, 0
            bstrpick.d  $t1, $t0, 7, 0
            movgr2cf    $fcc0, $t1
            bstrpick.d  $t1, $t0, 15, 8
            movgr2cf    $fcc1, $t1
            bstrpick.d  $t1, $t0, 23, 16
            movgr2cf    $fcc2, $t1
            bstrpick.d  $t1, $t0, 31, 24
            movgr2cf    $fcc3, $t1
            bstrpick.d  $t1, $t0, 39, 32
            movgr2cf    $fcc4, $t1
            bstrpick.d  $t1, $t0, 47, 40
            movgr2cf    $fcc5, $t1
            bstrpick.d  $t1, $t0, 55, 48
            movgr2cf    $fcc6, $t1
            bstrpick.d  $t1, $t0, 63, 56
            movgr2cf    $fcc7, $t1
        .endm

        .macro SAVE_FCSR, base
            movfcsr2gr  $t0, $fcsr0
            st.w        $t0, \base, 0
        .endm

        .macro RESTORE_FCSR, base
            ld.w        $t0, \base, 0
            movgr2fcsr  $fcsr0, $t0
        .endm

        // LoongArch64 specific floating point macros
        .macro PUSH_POP_FLOAT_REGS, op, base_reg
            \op $f0,  \base_reg, 0*8
            \op $f1,  \base_reg, 1*8
            \op $f2,  \base_reg, 2*8
            \op $f3,  \base_reg, 3*8
            \op $f4,  \base_reg, 4*8
            \op $f5,  \base_reg, 5*8
            \op $f6,  \base_reg, 6*8
            \op $f7,  \base_reg, 7*8
            \op $f8,  \base_reg, 8*8
            \op $f9,  \base_reg, 9*8
            \op $f10, \base_reg, 10*8
            \op $f11, \base_reg, 11*8
            \op $f12, \base_reg, 12*8
            \op $f13, \base_reg, 13*8
            \op $f14, \base_reg, 14*8
            \op $f15, \base_reg, 15*8
            \op $f16, \base_reg, 16*8
            \op $f17, \base_reg, 17*8
            \op $f18, \base_reg, 18*8
            \op $f19, \base_reg, 19*8
            \op $f20, \base_reg, 20*8
            \op $f21, \base_reg, 21*8
            \op $f22, \base_reg, 22*8
            \op $f23, \base_reg, 23*8
            \op $f24, \base_reg, 24*8
            \op $f25, \base_reg, 25*8
            \op $f26, \base_reg, 26*8
            \op $f27, \base_reg, 27*8
            \op $f28, \base_reg, 28*8
            \op $f29, \base_reg, 29*8
            \op $f30, \base_reg, 30*8
            \op $f31, \base_reg, 31*8
        .endm

        .macro SAVE_FP, base_reg
            PUSH_POP_FLOAT_REGS fst.d, \base_reg
        .endm

        .macro RESTORE_FP, base_reg
            PUSH_POP_FLOAT_REGS fld.d, \base_reg
        .endm

        // LSX 128-bit vector registers vr0-vr31 alias the scalar FP registers
        // f0-f31 in their low 64 bits. The macros below save/restore the HIGH
        // 64 bits (vr[127:64], element index 1 of the doubleword view) that the
        // scalar fst.d/fld.d above do not touch. LSX must be enabled in EUEN.SXE
        // (done at boot) for these to execute.
        //
        // `vstelm.d vd, rj, si8, idx` stores doubleword element `idx` of `vd`.
        // `vld`/`vinsgr2vr.d` are used on restore: vinsgr2vr.d inserts a GPR into
        // a vector element, leaving the other (low) element untouched, so it must
        // run AFTER the scalar fld.d that loads the low half.
        .macro SAVE_VR_HIGH vd, base_reg, off
            vstelm.d \vd, \base_reg, \off, 1
        .endm
        .macro RESTORE_VR_HIGH vd, base_reg, off
            ld.d        $t0, \base_reg, \off
            vinsgr2vr.d \vd, $t0, 1
        .endm

        .macro SAVE_FP_HIGH, base_reg
            SAVE_VR_HIGH $vr0,  \base_reg, 0*8
            SAVE_VR_HIGH $vr1,  \base_reg, 1*8
            SAVE_VR_HIGH $vr2,  \base_reg, 2*8
            SAVE_VR_HIGH $vr3,  \base_reg, 3*8
            SAVE_VR_HIGH $vr4,  \base_reg, 4*8
            SAVE_VR_HIGH $vr5,  \base_reg, 5*8
            SAVE_VR_HIGH $vr6,  \base_reg, 6*8
            SAVE_VR_HIGH $vr7,  \base_reg, 7*8
            SAVE_VR_HIGH $vr8,  \base_reg, 8*8
            SAVE_VR_HIGH $vr9,  \base_reg, 9*8
            SAVE_VR_HIGH $vr10, \base_reg, 10*8
            SAVE_VR_HIGH $vr11, \base_reg, 11*8
            SAVE_VR_HIGH $vr12, \base_reg, 12*8
            SAVE_VR_HIGH $vr13, \base_reg, 13*8
            SAVE_VR_HIGH $vr14, \base_reg, 14*8
            SAVE_VR_HIGH $vr15, \base_reg, 15*8
            SAVE_VR_HIGH $vr16, \base_reg, 16*8
            SAVE_VR_HIGH $vr17, \base_reg, 17*8
            SAVE_VR_HIGH $vr18, \base_reg, 18*8
            SAVE_VR_HIGH $vr19, \base_reg, 19*8
            SAVE_VR_HIGH $vr20, \base_reg, 20*8
            SAVE_VR_HIGH $vr21, \base_reg, 21*8
            SAVE_VR_HIGH $vr22, \base_reg, 22*8
            SAVE_VR_HIGH $vr23, \base_reg, 23*8
            SAVE_VR_HIGH $vr24, \base_reg, 24*8
            SAVE_VR_HIGH $vr25, \base_reg, 25*8
            SAVE_VR_HIGH $vr26, \base_reg, 26*8
            SAVE_VR_HIGH $vr27, \base_reg, 27*8
            SAVE_VR_HIGH $vr28, \base_reg, 28*8
            SAVE_VR_HIGH $vr29, \base_reg, 29*8
            SAVE_VR_HIGH $vr30, \base_reg, 30*8
            SAVE_VR_HIGH $vr31, \base_reg, 31*8
        .endm

        .macro RESTORE_FP_HIGH, base_reg
            RESTORE_VR_HIGH $vr0,  \base_reg, 0*8
            RESTORE_VR_HIGH $vr1,  \base_reg, 1*8
            RESTORE_VR_HIGH $vr2,  \base_reg, 2*8
            RESTORE_VR_HIGH $vr3,  \base_reg, 3*8
            RESTORE_VR_HIGH $vr4,  \base_reg, 4*8
            RESTORE_VR_HIGH $vr5,  \base_reg, 5*8
            RESTORE_VR_HIGH $vr6,  \base_reg, 6*8
            RESTORE_VR_HIGH $vr7,  \base_reg, 7*8
            RESTORE_VR_HIGH $vr8,  \base_reg, 8*8
            RESTORE_VR_HIGH $vr9,  \base_reg, 9*8
            RESTORE_VR_HIGH $vr10, \base_reg, 10*8
            RESTORE_VR_HIGH $vr11, \base_reg, 11*8
            RESTORE_VR_HIGH $vr12, \base_reg, 12*8
            RESTORE_VR_HIGH $vr13, \base_reg, 13*8
            RESTORE_VR_HIGH $vr14, \base_reg, 14*8
            RESTORE_VR_HIGH $vr15, \base_reg, 15*8
            RESTORE_VR_HIGH $vr16, \base_reg, 16*8
            RESTORE_VR_HIGH $vr17, \base_reg, 17*8
            RESTORE_VR_HIGH $vr18, \base_reg, 18*8
            RESTORE_VR_HIGH $vr19, \base_reg, 19*8
            RESTORE_VR_HIGH $vr20, \base_reg, 20*8
            RESTORE_VR_HIGH $vr21, \base_reg, 21*8
            RESTORE_VR_HIGH $vr22, \base_reg, 22*8
            RESTORE_VR_HIGH $vr23, \base_reg, 23*8
            RESTORE_VR_HIGH $vr24, \base_reg, 24*8
            RESTORE_VR_HIGH $vr25, \base_reg, 25*8
            RESTORE_VR_HIGH $vr26, \base_reg, 26*8
            RESTORE_VR_HIGH $vr27, \base_reg, 27*8
            RESTORE_VR_HIGH $vr28, \base_reg, 28*8
            RESTORE_VR_HIGH $vr29, \base_reg, 29*8
            RESTORE_VR_HIGH $vr30, \base_reg, 30*8
            RESTORE_VR_HIGH $vr31, \base_reg, 31*8
        .endm

        // LASX 256-bit vector registers xr0-xr31 extend LSX vr0-vr31 with two
        // additional doubleword elements. The low elements are restored first by
        // RESTORE_FP/RESTORE_FP_HIGH; the LASX restore macros only insert
        // elements 2 and 3 and leave the low 128 bits untouched.
        .macro SAVE_XR_ELEM xd, base_reg, off, idx
            xvstelm.d \xd, \base_reg, \off, \idx
        .endm
        .macro RESTORE_XR_ELEM xd, base_reg, off, idx
            ld.d          $t0, \base_reg, \off
            xvinsgr2vr.d  \xd, $t0, \idx
        .endm

        .macro SAVE_FP_LASX_HI0, base_reg
            SAVE_XR_ELEM $xr0,  \base_reg, 0*8,  2
            SAVE_XR_ELEM $xr1,  \base_reg, 1*8,  2
            SAVE_XR_ELEM $xr2,  \base_reg, 2*8,  2
            SAVE_XR_ELEM $xr3,  \base_reg, 3*8,  2
            SAVE_XR_ELEM $xr4,  \base_reg, 4*8,  2
            SAVE_XR_ELEM $xr5,  \base_reg, 5*8,  2
            SAVE_XR_ELEM $xr6,  \base_reg, 6*8,  2
            SAVE_XR_ELEM $xr7,  \base_reg, 7*8,  2
            SAVE_XR_ELEM $xr8,  \base_reg, 8*8,  2
            SAVE_XR_ELEM $xr9,  \base_reg, 9*8,  2
            SAVE_XR_ELEM $xr10, \base_reg, 10*8, 2
            SAVE_XR_ELEM $xr11, \base_reg, 11*8, 2
            SAVE_XR_ELEM $xr12, \base_reg, 12*8, 2
            SAVE_XR_ELEM $xr13, \base_reg, 13*8, 2
            SAVE_XR_ELEM $xr14, \base_reg, 14*8, 2
            SAVE_XR_ELEM $xr15, \base_reg, 15*8, 2
            SAVE_XR_ELEM $xr16, \base_reg, 16*8, 2
            SAVE_XR_ELEM $xr17, \base_reg, 17*8, 2
            SAVE_XR_ELEM $xr18, \base_reg, 18*8, 2
            SAVE_XR_ELEM $xr19, \base_reg, 19*8, 2
            SAVE_XR_ELEM $xr20, \base_reg, 20*8, 2
            SAVE_XR_ELEM $xr21, \base_reg, 21*8, 2
            SAVE_XR_ELEM $xr22, \base_reg, 22*8, 2
            SAVE_XR_ELEM $xr23, \base_reg, 23*8, 2
            SAVE_XR_ELEM $xr24, \base_reg, 24*8, 2
            SAVE_XR_ELEM $xr25, \base_reg, 25*8, 2
            SAVE_XR_ELEM $xr26, \base_reg, 26*8, 2
            SAVE_XR_ELEM $xr27, \base_reg, 27*8, 2
            SAVE_XR_ELEM $xr28, \base_reg, 28*8, 2
            SAVE_XR_ELEM $xr29, \base_reg, 29*8, 2
            SAVE_XR_ELEM $xr30, \base_reg, 30*8, 2
            SAVE_XR_ELEM $xr31, \base_reg, 31*8, 2
        .endm

        .macro SAVE_FP_LASX_HI1, base_reg
            SAVE_XR_ELEM $xr0,  \base_reg, 0*8,  3
            SAVE_XR_ELEM $xr1,  \base_reg, 1*8,  3
            SAVE_XR_ELEM $xr2,  \base_reg, 2*8,  3
            SAVE_XR_ELEM $xr3,  \base_reg, 3*8,  3
            SAVE_XR_ELEM $xr4,  \base_reg, 4*8,  3
            SAVE_XR_ELEM $xr5,  \base_reg, 5*8,  3
            SAVE_XR_ELEM $xr6,  \base_reg, 6*8,  3
            SAVE_XR_ELEM $xr7,  \base_reg, 7*8,  3
            SAVE_XR_ELEM $xr8,  \base_reg, 8*8,  3
            SAVE_XR_ELEM $xr9,  \base_reg, 9*8,  3
            SAVE_XR_ELEM $xr10, \base_reg, 10*8, 3
            SAVE_XR_ELEM $xr11, \base_reg, 11*8, 3
            SAVE_XR_ELEM $xr12, \base_reg, 12*8, 3
            SAVE_XR_ELEM $xr13, \base_reg, 13*8, 3
            SAVE_XR_ELEM $xr14, \base_reg, 14*8, 3
            SAVE_XR_ELEM $xr15, \base_reg, 15*8, 3
            SAVE_XR_ELEM $xr16, \base_reg, 16*8, 3
            SAVE_XR_ELEM $xr17, \base_reg, 17*8, 3
            SAVE_XR_ELEM $xr18, \base_reg, 18*8, 3
            SAVE_XR_ELEM $xr19, \base_reg, 19*8, 3
            SAVE_XR_ELEM $xr20, \base_reg, 20*8, 3
            SAVE_XR_ELEM $xr21, \base_reg, 21*8, 3
            SAVE_XR_ELEM $xr22, \base_reg, 22*8, 3
            SAVE_XR_ELEM $xr23, \base_reg, 23*8, 3
            SAVE_XR_ELEM $xr24, \base_reg, 24*8, 3
            SAVE_XR_ELEM $xr25, \base_reg, 25*8, 3
            SAVE_XR_ELEM $xr26, \base_reg, 26*8, 3
            SAVE_XR_ELEM $xr27, \base_reg, 27*8, 3
            SAVE_XR_ELEM $xr28, \base_reg, 28*8, 3
            SAVE_XR_ELEM $xr29, \base_reg, 29*8, 3
            SAVE_XR_ELEM $xr30, \base_reg, 30*8, 3
            SAVE_XR_ELEM $xr31, \base_reg, 31*8, 3
        .endm

        .macro RESTORE_FP_LASX_HI0, base_reg
            RESTORE_XR_ELEM $xr0,  \base_reg, 0*8,  2
            RESTORE_XR_ELEM $xr1,  \base_reg, 1*8,  2
            RESTORE_XR_ELEM $xr2,  \base_reg, 2*8,  2
            RESTORE_XR_ELEM $xr3,  \base_reg, 3*8,  2
            RESTORE_XR_ELEM $xr4,  \base_reg, 4*8,  2
            RESTORE_XR_ELEM $xr5,  \base_reg, 5*8,  2
            RESTORE_XR_ELEM $xr6,  \base_reg, 6*8,  2
            RESTORE_XR_ELEM $xr7,  \base_reg, 7*8,  2
            RESTORE_XR_ELEM $xr8,  \base_reg, 8*8,  2
            RESTORE_XR_ELEM $xr9,  \base_reg, 9*8,  2
            RESTORE_XR_ELEM $xr10, \base_reg, 10*8, 2
            RESTORE_XR_ELEM $xr11, \base_reg, 11*8, 2
            RESTORE_XR_ELEM $xr12, \base_reg, 12*8, 2
            RESTORE_XR_ELEM $xr13, \base_reg, 13*8, 2
            RESTORE_XR_ELEM $xr14, \base_reg, 14*8, 2
            RESTORE_XR_ELEM $xr15, \base_reg, 15*8, 2
            RESTORE_XR_ELEM $xr16, \base_reg, 16*8, 2
            RESTORE_XR_ELEM $xr17, \base_reg, 17*8, 2
            RESTORE_XR_ELEM $xr18, \base_reg, 18*8, 2
            RESTORE_XR_ELEM $xr19, \base_reg, 19*8, 2
            RESTORE_XR_ELEM $xr20, \base_reg, 20*8, 2
            RESTORE_XR_ELEM $xr21, \base_reg, 21*8, 2
            RESTORE_XR_ELEM $xr22, \base_reg, 22*8, 2
            RESTORE_XR_ELEM $xr23, \base_reg, 23*8, 2
            RESTORE_XR_ELEM $xr24, \base_reg, 24*8, 2
            RESTORE_XR_ELEM $xr25, \base_reg, 25*8, 2
            RESTORE_XR_ELEM $xr26, \base_reg, 26*8, 2
            RESTORE_XR_ELEM $xr27, \base_reg, 27*8, 2
            RESTORE_XR_ELEM $xr28, \base_reg, 28*8, 2
            RESTORE_XR_ELEM $xr29, \base_reg, 29*8, 2
            RESTORE_XR_ELEM $xr30, \base_reg, 30*8, 2
            RESTORE_XR_ELEM $xr31, \base_reg, 31*8, 2
        .endm

        .macro RESTORE_FP_LASX_HI1, base_reg
            RESTORE_XR_ELEM $xr0,  \base_reg, 0*8,  3
            RESTORE_XR_ELEM $xr1,  \base_reg, 1*8,  3
            RESTORE_XR_ELEM $xr2,  \base_reg, 2*8,  3
            RESTORE_XR_ELEM $xr3,  \base_reg, 3*8,  3
            RESTORE_XR_ELEM $xr4,  \base_reg, 4*8,  3
            RESTORE_XR_ELEM $xr5,  \base_reg, 5*8,  3
            RESTORE_XR_ELEM $xr6,  \base_reg, 6*8,  3
            RESTORE_XR_ELEM $xr7,  \base_reg, 7*8,  3
            RESTORE_XR_ELEM $xr8,  \base_reg, 8*8,  3
            RESTORE_XR_ELEM $xr9,  \base_reg, 9*8,  3
            RESTORE_XR_ELEM $xr10, \base_reg, 10*8, 3
            RESTORE_XR_ELEM $xr11, \base_reg, 11*8, 3
            RESTORE_XR_ELEM $xr12, \base_reg, 12*8, 3
            RESTORE_XR_ELEM $xr13, \base_reg, 13*8, 3
            RESTORE_XR_ELEM $xr14, \base_reg, 14*8, 3
            RESTORE_XR_ELEM $xr15, \base_reg, 15*8, 3
            RESTORE_XR_ELEM $xr16, \base_reg, 16*8, 3
            RESTORE_XR_ELEM $xr17, \base_reg, 17*8, 3
            RESTORE_XR_ELEM $xr18, \base_reg, 18*8, 3
            RESTORE_XR_ELEM $xr19, \base_reg, 19*8, 3
            RESTORE_XR_ELEM $xr20, \base_reg, 20*8, 3
            RESTORE_XR_ELEM $xr21, \base_reg, 21*8, 3
            RESTORE_XR_ELEM $xr22, \base_reg, 22*8, 3
            RESTORE_XR_ELEM $xr23, \base_reg, 23*8, 3
            RESTORE_XR_ELEM $xr24, \base_reg, 24*8, 3
            RESTORE_XR_ELEM $xr25, \base_reg, 25*8, 3
            RESTORE_XR_ELEM $xr26, \base_reg, 26*8, 3
            RESTORE_XR_ELEM $xr27, \base_reg, 27*8, 3
            RESTORE_XR_ELEM $xr28, \base_reg, 28*8, 3
            RESTORE_XR_ELEM $xr29, \base_reg, 29*8, 3
            RESTORE_XR_ELEM $xr30, \base_reg, 30*8, 3
            RESTORE_XR_ELEM $xr31, \base_reg, 31*8, 3
        .endm

        .endif"#
    };
}
