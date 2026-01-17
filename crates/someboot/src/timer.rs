use core::time::Duration;

use crate::ArchTrait;

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Enable the platform system timer so that timer IRQs can fire.
pub fn enable() {
    crate::arch::Arch::systimer_enable();
}

/// Disable the platform system timer to stop timer IRQs.
pub fn irq_disable() {
    crate::arch::Arch::systimer_irq_disable();
}

pub fn irq_enable() {
    crate::arch::Arch::systimer_irq_enable();
}

pub fn irq_is_enabled() -> bool {
    crate::arch::Arch::systimer_irq_is_enabled()
}

/// Configure the system timer with the desired interval.
pub fn set_next_event(interval: Duration) {
    let ticks = duration_to_ticks(interval);
    crate::arch::Arch::systimer_set_interval(ticks);
}

pub fn set_next_event_in_ticks(ticks: usize) {
    crate::arch::Arch::systimer_set_interval(ticks);
}

/// Acknowledge and clear the timer interrupt.
/// This must be called in the timer interrupt handler.
pub fn ack() {
    crate::arch::Arch::systimer_ack();
}

pub fn since_boot() -> Duration {
    elapsed()
}

/// Get the timer frequency in Hz.
#[inline]
pub fn freq() -> usize {
    crate::arch::Arch::systimer_freq()
}

/// Get the current timer tick count.
#[inline]
pub fn ticks() -> usize {
    crate::arch::Arch::systimer_tick()
}

/// Convert ticks to Duration.
#[inline]
pub fn ticks_to_duration(ticks: usize) -> Duration {
    let freq = freq();
    if freq == 0 {
        return Duration::ZERO;
    }
    // ticks * 1_000_000_000 / freq
    // Use u128 to avoid overflow
    let nanos = (ticks as u128 * NANOS_PER_SEC as u128) / freq as u128;
    Duration::from_nanos(nanos as u64)
}

/// Convert Duration to ticks.
#[inline]
pub fn duration_to_ticks(duration: Duration) -> usize {
    let freq = freq();
    if freq == 0 {
        return 0;
    }
    // duration.as_nanos() * freq / 1_000_000_000
    // Use u128 to avoid overflow
    let ticks = (duration.as_nanos() * freq as u128) / NANOS_PER_SEC as u128;
    ticks as _
}

/// Get the elapsed time since boot.
#[inline]
pub fn elapsed() -> Duration {
    ticks_to_duration(ticks())
}
