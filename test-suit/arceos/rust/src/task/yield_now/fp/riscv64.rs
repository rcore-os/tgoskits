//! fs0 is callee-saved by the RISC-V floating-point ABI.
#[inline(always)]
pub(super) fn read() -> u64 {
    let bits;
    // SAFETY: reads one FP register without accessing memory.
    unsafe {
        core::arch::asm!("fmv.x.d {bits}, fs0",bits=out(reg)bits,options(nomem,nostack));
    }
    bits
}
#[inline(always)]
pub(super) fn write(bits: u64) {
    // SAFETY: the shared probe restores the entry value before returning.
    unsafe {
        core::arch::asm!("fmv.d.x fs0, {bits}",bits=in(reg)bits,out("fs0") _,options(nomem,nostack));
    }
}
pub(super) fn value(_: u64, index: usize) -> u64 {
    0x3ff1_0000_0000_0000 + index as u64
}
pub(super) fn matches(actual: u64, expected: u64) -> bool {
    actual == expected
}
