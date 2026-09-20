//! Shared native and LVZ FP/vector state.

#[cfg(feature = "fp-simd")]
use core::{arch::naked_asm, mem::offset_of};

/// Floating-point / LSX/LASX vector registers of LoongArch64.
///
/// The platform enables the LSX 128-bit and LASX 256-bit vector extensions at
/// boot, so user code may use the full 256-bit vector registers `xr0`-`xr31`.
/// The scalar FP registers `f0`-`f31` alias the low 64 bits of `vr0`-`vr31`,
/// and `vr0`-`vr31` alias the low 128 bits of `xr0`-`xr31`. To correctly
/// save/restore the live FP/vector state across a context switch or a signal
/// delivery we must preserve all 256 bits of each register, not just the scalar
/// low 64 bits.
///
/// `fp` holds the low 64 bits (`xr[63:0]`, i.e. the scalar `fN` view),
/// `fp_high` holds `xr[127:64]`, and the LASX-only fields hold the upper
/// `xr[255:128]`. Keeping `fp` first and with its original layout means any
/// consumer that treated `fp[i]` as the scalar double `fN` still observes the
/// same value; the vector extension fields are additive.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct FpuState {
    /// Low 64 bits of the vector registers `vr0`-`vr31` (the scalar `f0`-`f31`).
    pub fp: [u64; 32],
    /// High 64 bits of the vector registers `vr0`-`vr31` (`vr[127:64]`).
    ///
    /// Saved/restored via LSX so an async signal (or preemptive switch)
    /// interrupting a thread mid-LSX-op does not clobber these on resume.
    pub fp_high: [u64; 32],
    /// LASX-only doubleword element 2 (`xr[191:128]`) of `xr0`-`xr31`.
    pub fp_lasx_hi0: [u64; 32],
    /// LASX-only doubleword element 3 (`xr[255:192]`) of `xr0`-`xr31`.
    pub fp_lasx_hi1: [u64; 32],
    /// Floating-point Condition Code register
    pub fcc: [u8; 8],
    /// Floating-point Control and Status register
    pub fcsr: u32,
}

#[cfg(feature = "fp-simd")]
impl FpuState {
    /// Save the current FPU states from CPU to this structure.
    #[inline]
    pub fn save(&mut self) {
        unsafe { save_fp_registers(self) }
    }

    /// Restore FPU states from this structure to CPU.
    #[inline]
    pub fn restore(&self) {
        unsafe { restore_fp_registers(self) }
    }
}

#[cfg(feature = "fp-simd")]
#[unsafe(naked)]
pub(super) unsafe extern "C" fn save_fp_registers(fpu: &mut FpuState) {
    naked_asm!(
        include_fp_asm_macros!(),
        "
        SAVE_FP $a0
        addi.d $t8, $a0, {fp_high_offset}
        SAVE_FP_HIGH $t8

        csrrd $t0, 0x2
        andi $t0, $t0, 0x4
        beqz $t0, 1f
        addi.d $t8, $a0, {fp_lasx_hi0_offset}
        SAVE_FP_LASX_HI0 $t8
        addi.d $t8, $a0, {fp_lasx_hi1_offset}
        SAVE_FP_LASX_HI1 $t8
1:
        addi.d $t8, $a0, {fcc_offset}
        SAVE_FCC $t8
        addi.d $t8, $a0, {fcsr_offset}
        SAVE_FCSR $t8
        ret",
        fp_high_offset = const offset_of!(FpuState, fp_high),
        fp_lasx_hi0_offset = const offset_of!(FpuState, fp_lasx_hi0),
        fp_lasx_hi1_offset = const offset_of!(FpuState, fp_lasx_hi1),
        fcc_offset = const offset_of!(FpuState, fcc),
        fcsr_offset = const offset_of!(FpuState, fcsr),
    )
}

#[cfg(feature = "fp-simd")]
#[unsafe(naked)]
pub(super) unsafe extern "C" fn restore_fp_registers(fpu: &FpuState) {
    naked_asm!(
        include_fp_asm_macros!(),
        "
        RESTORE_FP $a0
        addi.d $t8, $a0, {fp_high_offset}
        RESTORE_FP_HIGH $t8

        csrrd $t0, 0x2
        andi $t0, $t0, 0x4
        beqz $t0, 1f
        addi.d $t8, $a0, {fp_lasx_hi0_offset}
        RESTORE_FP_LASX_HI0 $t8
        addi.d $t8, $a0, {fp_lasx_hi1_offset}
        RESTORE_FP_LASX_HI1 $t8
1:
        addi.d $t8, $a0, {fcc_offset}
        RESTORE_FCC $t8
        addi.d $t8, $a0, {fcsr_offset}
        RESTORE_FCSR $t8
        ret",
        fp_high_offset = const offset_of!(FpuState, fp_high),
        fp_lasx_hi0_offset = const offset_of!(FpuState, fp_lasx_hi0),
        fp_lasx_hi1_offset = const offset_of!(FpuState, fp_lasx_hi1),
        fcc_offset = const offset_of!(FpuState, fcc),
        fcsr_offset = const offset_of!(FpuState, fcsr),
    )
}
