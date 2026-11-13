//! AAPCS64 preserves the low 64 bits of v8 across function calls.
#[inline(always)]
pub(super) fn read() -> u64 {
    let bits;
    // SAFETY: reads d8 without accessing memory.
    unsafe {
        core::arch::asm!(".arch_extension fp", "fmov {bits}, d8",bits=out(reg)bits,options(nomem,nostack));
    }
    bits
}
#[inline(always)]
pub(super) fn write(bits: u64) {
    // SAFETY: the shared probe restores d8's entry bits before returning.
    unsafe {
        core::arch::asm!(".arch_extension fp", "fmov d8, {bits}",bits=in(reg)bits,out("v8") _,options(nomem,nostack));
    }
}
pub(super) fn value(_: u64, index: usize) -> u64 {
    0x3ff1_0000_0000_0000 + index as u64
}
pub(super) fn matches(actual: u64, expected: u64) -> bool {
    actual == expected
}
