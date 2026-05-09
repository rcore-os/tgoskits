use core::arch::asm;

const IE_MASK: usize = 1 << 2;

#[inline]
pub fn local_irq_save_and_disable() -> usize {
    let mut flags: usize = 0;
    // clear the `IE` bit, and return the old CSR
    unsafe { asm!("csrxchg {}, $t0, 0x0", inout(reg) flags, in("$t0") IE_MASK) };
    flags & IE_MASK
}

#[inline]
pub fn local_irq_restore(flags: usize) {
    // restore the `IE` bit
    unsafe { asm!("csrxchg {}, $t0, 0x0", in(reg) flags, in("$t0") IE_MASK) };
}
