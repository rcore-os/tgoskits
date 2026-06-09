#[inline]
pub fn local_irq_save_and_disable() -> usize {
    0
}

#[inline]
pub fn local_irq_restore(flags: usize) {
    let _ = flags;
}
