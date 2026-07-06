//! Task APIs for single-task configuration.

/// For single-task situation, we just relax the CPU and wait for incoming
/// interrupts.
pub fn yield_now() {
    ax_hal::asm::wait_for_irqs();
}

/// For single-task situation, we just busy wait for the given duration.
pub fn sleep(dur: core::time::Duration) {
    ax_hal::time::busy_wait(dur);
}

/// For single-task situation, we just busy wait until reaching the given
/// monotonic deadline.
pub fn sleep_until(deadline: ax_hal::time::TimeValue) {
    ax_hal::time::busy_wait_until(deadline);
}
