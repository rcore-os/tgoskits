use core::time::Duration;

use crate::ArchTrait;

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Hardware counter contract exposed to the platform scheduler clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterStability {
    /// Every runtime CPU observes one synchronized system counter.
    Stable,
    /// The counter is CPU-local and requires per-CPU correction.
    Unstable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ArchTimerMode {
    El1Phys    = 0,
    El1Virt    = 1,
    El2HypPhys = 2,
}

impl ArchTimerMode {
    pub const fn from_raw(raw: u8) -> Self {
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

#[cfg(any(target_arch = "aarch64", test))]
pub(crate) fn resume_masked_level_oneshot(
    program_comparator: impl FnOnce(),
    unmask_source: impl FnOnce(),
) {
    program_comparator();
    unmask_source();
}

/// Keeps platform clock-event deadlines ahead of the current raw counter.
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64", test))]
pub(crate) fn next_cpu_timer_deadline(now: u64, requested: u64) -> u64 {
    requested.max(now.saturating_add(1))
}

#[cfg(any(target_arch = "riscv64", test))]
pub(crate) mod riscv64_interval {
    /// Returns the SBI comparator value used to disarm a one-shot timer.
    pub(crate) const fn stopped_deadline() -> u64 {
        u64::MAX
    }
}

#[cfg(any(target_arch = "loongarch64", test))]
pub(crate) mod loongarch64_interval {
    const ALIGNMENT: usize = 4;
    const MIN_TICKS: usize = 4;

    /// Converts a relative interval to the bounded 4-tick value encoded by TCFG.
    pub(crate) fn aligned_ticks(interval_ticks: usize) -> usize {
        let max_aligned = usize::MAX - usize::MAX % ALIGNMENT;
        let clamped = interval_ticks.max(MIN_TICKS).min(max_aligned);
        (clamped + (ALIGNMENT - 1)) & !(ALIGNMENT - 1)
    }

    /// Returns the largest valid one-shot interval encoded by TCFG.
    pub(crate) const fn stopped_ticks() -> usize {
        usize::MAX & !(ALIGNMENT - 1)
    }
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

/// Reports whether scheduler users may sample the raw counter on any CPU.
#[inline]
pub fn scheduler_clock_stability() -> CounterStability {
    crate::arch::Arch::systimer_stability()
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
    use core::cell::Cell;

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

    #[test]
    fn masked_level_timer_replaces_the_comparator_before_unmask() {
        let step = Cell::new(0);

        resume_masked_level_oneshot(
            || assert_eq!(step.replace(1), 0),
            || assert_eq!(step.replace(2), 1),
        );

        assert_eq!(step.get(), 2);
    }

    #[test]
    fn riscv64_stopped_deadline_disarms_comparator() {
        assert_eq!(riscv64_interval::stopped_deadline(), u64::MAX);
    }

    #[test]
    fn loongarch64_interval_clamps_before_rounding() {
        assert_eq!(loongarch64_interval::aligned_ticks(1), 4);
        assert_eq!(loongarch64_interval::aligned_ticks(5), 8);
        assert_eq!(
            loongarch64_interval::aligned_ticks(usize::MAX),
            usize::MAX & !3
        );
        assert_eq!(loongarch64_interval::stopped_ticks(), usize::MAX & !3);
    }

    #[test]
    fn clock_event_deadlines_remain_ahead_of_the_counter() {
        assert_eq!(next_cpu_timer_deadline(17, 23), 23);
        assert_eq!(next_cpu_timer_deadline(19, 8), 20);
        assert_eq!(next_cpu_timer_deadline(19, 19), 20);
        assert_eq!(next_cpu_timer_deadline(u64::MAX, 0), u64::MAX);
    }
}
