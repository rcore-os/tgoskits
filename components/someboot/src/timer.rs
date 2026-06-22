use core::time::Duration;

use crate::ArchTrait;

const NANOS_PER_SEC: u64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ArchTimerMode {
    El1Phys    = 0,
    El1Virt    = 1,
    El2HypPhys = 2,
}

impl ArchTimerMode {
    const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::El1Virt,
            2 => Self::El2HypPhys,
            _ => Self::El1Phys,
        }
    }
}

static mut ARCH_TIMER_MODE: u8 = ArchTimerMode::El1Phys as u8;

pub const fn select_aarch64_timer_mode(kernel_in_el2: bool, el2_available: bool) -> ArchTimerMode {
    if kernel_in_el2 {
        ArchTimerMode::El2HypPhys
    } else if el2_available {
        ArchTimerMode::El1Phys
    } else {
        ArchTimerMode::El1Virt
    }
}

pub const fn aarch64_timer_irq_index(mode: ArchTimerMode) -> usize {
    match mode {
        ArchTimerMode::El1Phys => 1,
        ArchTimerMode::El1Virt => 2,
        ArchTimerMode::El2HypPhys => 3,
    }
}

pub fn set_aarch64_timer_mode(mode: ArchTimerMode) {
    // Written once by the primary CPU during early boot before secondary CPUs run.
    unsafe { ARCH_TIMER_MODE = mode as u8 };
}

pub fn aarch64_timer_mode() -> ArchTimerMode {
    // After early boot this mode is read-only platform state.
    unsafe { ArchTimerMode::from_raw(ARCH_TIMER_MODE) }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el2_kernel_uses_hyp_physical_timer() {
        assert_eq!(
            select_aarch64_timer_mode(true, true),
            ArchTimerMode::El2HypPhys
        );
        assert_eq!(
            select_aarch64_timer_mode(true, false),
            ArchTimerMode::El2HypPhys
        );
    }

    #[test]
    fn el1_kernel_uses_physical_timer_when_el2_is_available() {
        assert_eq!(
            select_aarch64_timer_mode(false, true),
            ArchTimerMode::El1Phys
        );
    }

    #[test]
    fn el1_kernel_uses_virtual_timer_when_el2_is_unavailable() {
        assert_eq!(
            select_aarch64_timer_mode(false, false),
            ArchTimerMode::El1Virt
        );
    }

    #[test]
    fn timer_mode_maps_to_fdt_interrupt_index() {
        assert_eq!(aarch64_timer_irq_index(ArchTimerMode::El1Phys), 1);
        assert_eq!(aarch64_timer_irq_index(ArchTimerMode::El1Virt), 2);
        assert_eq!(aarch64_timer_irq_index(ArchTimerMode::El2HypPhys), 3);
    }
}
