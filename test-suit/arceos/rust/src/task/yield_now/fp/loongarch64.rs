//! f24 is the ABI-preserved fs0 register on LoongArch64.
#[inline(always)]
pub(super) fn read() -> u64 {
    let bits;
    // SAFETY: reads f24 without accessing memory.
    unsafe {
        core::arch::asm!("movfr2gr.d {bits}, $f24",bits=out(reg)bits,options(nomem,nostack));
    }
    bits
}
#[inline(always)]
pub(super) fn write(bits: u64) {
    // SAFETY: the shared probe restores f24's entry bits before returning.
    unsafe {
        core::arch::asm!("movgr2fr.d $f24, {bits}",bits=in(reg)bits,out("$f24") _,options(nomem,nostack));
    }
}
pub(super) fn value(_: u64, index: usize) -> u64 {
    0x3ff1_0000_0000_0000 + index as u64
}
pub(super) fn matches(actual: u64, expected: u64) -> bool {
    actual == expected
}
