//! Native FP/SIMD register layout and assembly used by task and guest contexts.

#[cfg(feature = "fp-simd")]
use core::{arch::naked_asm, mem::offset_of};

/// FP & SIMD registers.
#[repr(C, align(16))]
#[derive(Debug, Default)]
pub struct FpState {
    /// 128-bit SIMD & FP registers (V0..V31)
    pub regs: [u128; 32],
    /// Floating-point Control Register (FPCR)
    pub fpcr: u32,
    /// Floating-point Status Register (FPSR)
    pub fpsr: u32,
}

#[cfg(feature = "fp-simd")]
impl FpState {
    /// Saves the current FP/SIMD states from CPU to this structure.
    pub fn save(&mut self) {
        unsafe { fpstate_save(self) }
    }

    /// Restores the FP/SIMD states from this structure to CPU.
    pub fn restore(&self) {
        unsafe { fpstate_restore(self) }
    }
}

#[unsafe(naked)]
#[cfg(feature = "fp-simd")]
pub(super) unsafe extern "C" fn fpstate_save(state: &mut FpState) {
    naked_asm!(
        ".arch armv8
        // save fp/neon context
        mrs     x9, fpcr
        mrs     x10, fpsr
        stp     q0, q1, [x0, 0 * 16]
        stp     q2, q3, [x0, 2 * 16]
        stp     q4, q5, [x0, 4 * 16]
        stp     q6, q7, [x0, 6 * 16]
        stp     q8, q9, [x0, 8 * 16]
        stp     q10, q11, [x0, 10 * 16]
        stp     q12, q13, [x0, 12 * 16]
        stp     q14, q15, [x0, 14 * 16]
        stp     q16, q17, [x0, 16 * 16]
        stp     q18, q19, [x0, 18 * 16]
        stp     q20, q21, [x0, 20 * 16]
        stp     q22, q23, [x0, 22 * 16]
        stp     q24, q25, [x0, 24 * 16]
        stp     q26, q27, [x0, 26 * 16]
        stp     q28, q29, [x0, 28 * 16]
        stp     q30, q31, [x0, 30 * 16]
        str     w9, [x0, {fpcr_offset}]
        str     w10, [x0, {fpsr_offset}]

        isb
        ret",
        fpcr_offset = const offset_of!(FpState, fpcr),
        fpsr_offset = const offset_of!(FpState, fpsr),
    )
}

#[unsafe(naked)]
#[cfg(feature = "fp-simd")]
pub(super) unsafe extern "C" fn fpstate_restore(state: &FpState) {
    naked_asm!(
        ".arch armv8
        // restore fp/neon context
        ldp     q0, q1, [x0, 0 * 16]
        ldp     q2, q3, [x0, 2 * 16]
        ldp     q4, q5, [x0, 4 * 16]
        ldp     q6, q7, [x0, 6 * 16]
        ldp     q8, q9, [x0, 8 * 16]
        ldp     q10, q11, [x0, 10 * 16]
        ldp     q12, q13, [x0, 12 * 16]
        ldp     q14, q15, [x0, 14 * 16]
        ldp     q16, q17, [x0, 16 * 16]
        ldp     q18, q19, [x0, 18 * 16]
        ldp     q20, q21, [x0, 20 * 16]
        ldp     q22, q23, [x0, 22 * 16]
        ldp     q24, q25, [x0, 24 * 16]
        ldp     q26, q27, [x0, 26 * 16]
        ldp     q28, q29, [x0, 28 * 16]
        ldp     q30, q31, [x0, 30 * 16]
        ldr     w9, [x0, {fpcr_offset}]
        ldr     w10, [x0, {fpsr_offset}]
        msr     fpcr, x9
        msr     fpsr, x10

        isb
        ret",
        fpcr_offset = const offset_of!(FpState, fpcr),
        fpsr_offset = const offset_of!(FpState, fpsr),
    )
}
