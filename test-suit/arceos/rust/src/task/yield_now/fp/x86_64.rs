//! MXCSR control bits are callee-preserved in the SysV x86-64 ABI.
//! XMM registers are caller-saved, so keeping a value in one across an ordinary
//! Rust call would test an invalid calling-convention assumption.
const ROUNDING: u64 = 3 << 13;
#[inline(always)]
pub(super) fn read() -> u64 {
    let mut bits = 0u32;
    // SAFETY: STMXCSR writes exactly this live, aligned four-byte object.
    unsafe {
        core::arch::asm!("stmxcsr [{ptr}]",ptr=in(reg)&mut bits,options(nostack,preserves_flags));
    }
    u64::from(bits)
}
#[inline(always)]
pub(super) fn write(bits: u64) {
    let bits = bits as u32;
    // SAFETY: only rounding control is changed from a hardware-read valid
    // MXCSR image; the probe restores the original image before returning.
    unsafe {
        core::arch::asm!("ldmxcsr [{ptr}]",ptr=in(reg)&bits,options(nostack,preserves_flags));
    }
}
pub(super) fn value(previous: u64, index: usize) -> u64 {
    (previous & !ROUNDING) | ((index as u64 + 1) << 13)
}
pub(super) fn matches(actual: u64, expected: u64) -> bool {
    (actual & ROUNDING) == (expected & ROUNDING)
}
