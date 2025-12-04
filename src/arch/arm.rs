use core::arch::asm;

/// Bit 7: IRQ disable bit in CPSR
const IRQ_DISABLE_BIT: usize = 1 << 7;

#[inline]
pub fn local_irq_save_and_disable() -> usize {
    let flags: usize;
    unsafe {
        // Save CPSR and disable IRQs by setting the I bit
        asm!(
            "mrs {0}, cpsr",
            "cpsid i",
            out(reg) flags,
            options(nomem, nostack, preserves_flags)
        );
    }
    flags & IRQ_DISABLE_BIT
}

#[inline]
pub fn local_irq_restore(flags: usize) {
    if flags & IRQ_DISABLE_BIT == 0 {
        // IRQs were enabled before, re-enable them
        unsafe {
            asm!("cpsie i", options(nomem, nostack));
        }
    }
}
