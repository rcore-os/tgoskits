//! ARM Generic Timer.

use core::sync::atomic::{AtomicU64, Ordering};

const UNINIT_EPOCH_OFFSET_NANOS: u64 = u64::MAX;
static EPOCH_OFFSET_NANOS: AtomicU64 = AtomicU64::new(UNINIT_EPOCH_OFFSET_NANOS);
// The platform timer frequency is fixed for the lifetime of a boot. Cache it
// after the first conversion so every clock read does not repeat the provider
// lookup; the arithmetic and saturation semantics remain unchanged.
const UNINIT_FREQUENCY_HZ: u64 = u64::MAX;
static TIMER_FREQUENCY_HZ: AtomicU64 = AtomicU64::new(UNINIT_FREQUENCY_HZ);

#[inline(always)]
pub(crate) fn current_ticks() -> u64 {
    somehal::timer::ticks() as _
}

#[inline(always)]
fn timer_frequency_hz() -> u64 {
    let cached = TIMER_FREQUENCY_HZ.load(Ordering::Acquire);
    if cached != UNINIT_FREQUENCY_HZ {
        return cached;
    }
    let frequency = somehal::timer::freq() as u64;
    let _ = TIMER_FREQUENCY_HZ.compare_exchange(
        UNINIT_FREQUENCY_HZ,
        frequency,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
    frequency
}

#[inline(always)]
pub(crate) fn ticks_to_nanos(ticks: u64) -> u64 {
    let freq = timer_frequency_hz();
    ticks_to_nanos_at_frequency(ticks, freq)
}

#[inline(always)]
pub(crate) fn nanos_to_ticks(nanos: u64) -> u64 {
    let freq = timer_frequency_hz();
    nanos_to_ticks_at_frequency(nanos, freq)
}

const fn ticks_to_nanos_at_frequency(ticks: u64, frequency_hz: u64) -> u64 {
    if frequency_hz == 0 {
        return 0;
    }
    let nanos = (ticks as u128 * ax_plat::time::NANOS_PER_SEC as u128) / frequency_hz as u128;
    if nanos > u64::MAX as u128 {
        u64::MAX
    } else {
        nanos as u64
    }
}

const fn nanos_to_ticks_at_frequency(nanos: u64, frequency_hz: u64) -> u64 {
    if frequency_hz == 0 {
        return 0;
    }
    let ticks = (nanos as u128 * frequency_hz as u128) / ax_plat::time::NANOS_PER_SEC as u128;
    if ticks > u64::MAX as u128 {
        u64::MAX
    } else {
        ticks as u64
    }
}
const fn deadline_nanos_to_ticks_at_frequency(nanos: u64, frequency_hz: u64) -> u64 {
    if frequency_hz == 0 {
        return 0;
    }
    let scaled = nanos as u128 * frequency_hz as u128;
    let divisor = ax_plat::time::NANOS_PER_SEC as u128;
    let ticks = scaled / divisor + if scaled.is_multiple_of(divisor) { 0 } else { 1 };
    if ticks > u64::MAX as u128 {
        u64::MAX
    } else {
        ticks as u64
    }
}
fn oneshot_interval_ticks(deadline_ns: u64, current_ticks: u64, frequency_hz: u64) -> usize {
    let deadline_ticks = deadline_nanos_to_ticks_at_frequency(deadline_ns, frequency_hz);
    let delta = deadline_ticks.saturating_sub(current_ticks).max(1);
    if delta > usize::MAX as u64 {
        usize::MAX
    } else {
        delta as usize
    }
}
fn program_oneshot(
    deadline_ns: u64,
    current_ticks: u64,
    frequency_hz: u64,
    program_interval: impl FnOnce(usize),
) {
    let interval = oneshot_interval_ticks(deadline_ns, current_ticks, frequency_hz);
    program_interval(interval);
}
fn resume_oneshot(
    deadline_ns: u64,
    current_ticks: u64,
    frequency_hz: u64,
    resume_interval: impl FnOnce(usize),
) {
    let interval = oneshot_interval_ticks(deadline_ns, current_ticks, frequency_hz);
    resume_interval(interval);
}

pub fn try_init_epoch_offset(epoch_time_nanos: u64) -> bool {
    let boot_offset = epoch_time_nanos.saturating_sub(ticks_to_nanos(current_ticks()));
    EPOCH_OFFSET_NANOS
        .compare_exchange(
            UNINIT_EPOCH_OFFSET_NANOS,
            boot_offset,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

#[cfg(all(feature = "rtc", target_arch = "loongarch64"))]
pub(crate) fn try_init_epoch_offset_from_firmware() -> bool {
    let Some(epoch_time_nanos) = somehal::rtc::epoch_time_nanos() else {
        debug!("axplat-dyn: firmware RTC is not available");
        return false;
    };

    if try_init_epoch_offset(epoch_time_nanos) {
        info!("axplat-dyn: initialized wall clock from firmware RTC");
        true
    } else {
        debug!("axplat-dyn: firmware RTC skipped because epoch offset is already initialized");
        false
    }
}

struct GenericTimer;

#[impl_plat_interface]
impl ax_plat::time::TimeIf for GenericTimer {
    /// Returns the current clock time in hardware ticks.
    fn current_ticks() -> u64 {
        current_ticks()
    }

    /// Converts hardware ticks to nanoseconds.
    fn ticks_to_nanos(ticks: u64) -> u64 {
        ticks_to_nanos(ticks)
    }

    /// Converts nanoseconds to hardware ticks.
    fn nanos_to_ticks(nanos: u64) -> u64 {
        nanos_to_ticks(nanos)
    }

    fn scheduler_clock_stability() -> ax_plat::time::SchedulerClockStability {
        match somehal::timer::scheduler_clock_stability() {
            somehal::timer::CounterStability::Stable => {
                ax_plat::time::SchedulerClockStability::Stable
            }
            somehal::timer::CounterStability::Unstable => {
                ax_plat::time::SchedulerClockStability::Unstable
            }
        }
    }

    /// Return epoch offset in nanoseconds (wall time offset to monotonic
    /// clock start).
    fn epochoffset_nanos() -> u64 {
        match EPOCH_OFFSET_NANOS.load(Ordering::Acquire) {
            UNINIT_EPOCH_OFFSET_NANOS => 0,
            offset => offset,
        }
    }
    /// Returns the IRQ number for the timer interrupt.
    fn irq_num() -> ax_plat::irq::IrqId {
        somehal::irq::systick_irq()
    }
    /// Set a one-shot timer.
    ///
    /// A timer interrupt will be triggered at the specified monotonic time
    /// deadline (in nanoseconds).
    fn set_oneshot_timer(deadline_ns: u64) {
        let current_ticks = somehal::timer::ticks() as u64;
        let frequency_hz = timer_frequency_hz();
        program_oneshot(
            deadline_ns,
            current_ticks,
            frequency_hz,
            somehal::timer::set_next_event_in_ticks,
        );
    }
    fn resume_oneshot_timer(deadline_ns: u64) {
        let current_ticks = somehal::timer::ticks() as u64;
        let frequency_hz = timer_frequency_hz();
        resume_oneshot(
            deadline_ns,
            current_ticks,
            frequency_hz,
            somehal::timer::resume_oneshot_in_ticks,
        );
    }
    fn cancel_oneshot_timer() {
        somehal::timer::cancel_oneshot();
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::{
        deadline_nanos_to_ticks_at_frequency, nanos_to_ticks_at_frequency, oneshot_interval_ticks,
        program_oneshot, resume_oneshot, ticks_to_nanos_at_frequency,
    };

    #[test]
    fn nanosecond_conversion_saturates_instead_of_wrapping() {
        assert_eq!(nanos_to_ticks_at_frequency(u64::MAX, u64::MAX), u64::MAX);
        assert_eq!(ticks_to_nanos_at_frequency(u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn physical_deadline_conversion_rounds_up_to_the_next_tick() {
        assert_eq!(deadline_nanos_to_ticks_at_frequency(3, 500_000_000), 2);
        assert_eq!(nanos_to_ticks_at_frequency(3, 500_000_000), 1);
    }

    #[test]
    fn past_and_subtick_deadlines_use_the_minimum_interval() {
        assert_eq!(oneshot_interval_ticks(99, 100, 1_000_000_000), 1);
        assert_eq!(oneshot_interval_ticks(100, 100, 1_000_000_000), 1);
    }

    #[test]
    fn unrepresentable_tick_delta_clamps_to_the_device_argument() {
        assert_eq!(oneshot_interval_ticks(u64::MAX, 0, u64::MAX), usize::MAX);
    }

    #[test]
    fn resume_converts_the_absolute_deadline_once() {
        let resumed_interval = Cell::new(0);
        resume_oneshot(100, 0, 1_000_000_000, |interval| {
            resumed_interval.set(interval)
        });
        assert_eq!(resumed_interval.get(), 100);
    }

    #[test]
    fn resume_and_reprogram_each_install_one_comparator() {
        let program_count = Cell::new(0);
        resume_oneshot(100, 0, 1_000_000_000, |_| {
            program_count.set(program_count.get() + 1)
        });
        program_oneshot(200, 0, 1_000_000_000, |_| {
            program_count.set(program_count.get() + 1);
        });

        assert_eq!(program_count.get(), 2);
    }
}
