#[inline]
pub fn delay_ms(ms: u32) {
    for _ in 0..(ms as u64 * 100_000) {
        core::hint::spin_loop();
    }
}
