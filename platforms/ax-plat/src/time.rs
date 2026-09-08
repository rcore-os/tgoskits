//! Time-related operations.

use core::sync::atomic::{AtomicI64, Ordering};
pub use core::time::Duration;

/// A measurement of the system clock.
///
/// Currently, it reuses the [`core::time::Duration`] type. But it does not
/// represent a duration, but a clock time.
pub type TimeValue = Duration;

static WALL_TIME_ADJUSTMENT_NANOS: AtomicI64 = AtomicI64::new(0);

/// Number of milliseconds in a second.
pub const MILLIS_PER_SEC: u64 = 1_000;
/// Number of microseconds in a second.
pub const MICROS_PER_SEC: u64 = 1_000_000;
/// Number of nanoseconds in a second.
pub const NANOS_PER_SEC: u64 = 1_000_000_000;
/// Number of nanoseconds in a millisecond.
pub const NANOS_PER_MILLIS: u64 = 1_000_000;
/// Number of nanoseconds in a microsecond.
pub const NANOS_PER_MICROS: u64 = 1_000;

/// Platform assessment of the raw counter used by the scheduler clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerClockStability {
    /// Every CPU observes one synchronized system counter.
    Stable,
    /// The raw counter is CPU-local and requires per-CPU correction.
    Unstable,
}

/// Failure to access the platform scheduler clock lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SchedulerClockError {
    /// The logical CPU index is outside the installed per-CPU layout.
    #[error("logical CPU {cpu_id} is outside the installed per-CPU layout")]
    InvalidCpu { cpu_id: usize },
    /// The calling CPU has no validated CPU-local area yet.
    #[error("the calling CPU has no validated CPU-local area")]
    CurrentCpuUnavailable,
    /// An owner-only lifecycle operation was invoked from another CPU.
    #[error("scheduler clock CPU mismatch: expected {expected_cpu_id}, current {actual_cpu_id}")]
    WrongCurrentCpu {
        expected_cpu_id: usize,
        actual_cpu_id: usize,
    },
    /// The CPU scheduler clock is already online or being initialized.
    #[error("the scheduler clock CPU is already online")]
    CpuAlreadyOnline,
    /// The CPU scheduler clock is offline.
    #[error("the scheduler clock CPU is offline")]
    CpuOffline,
}

/// Failure to install a new wall-clock value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WallTimeError {
    /// The requested wall time precedes the current monotonic time.
    #[error("wall time cannot precede the current monotonic time")]
    BeforeMonotonic,
    /// The requested adjustment cannot be represented by the wall-clock state.
    #[error("wall-time adjustment is outside the supported range")]
    AdjustmentOutOfRange,
}

/// Time-related interfaces.
#[def_plat_interface]
pub trait TimeIf {
    /// Returns the current clock time in hardware ticks.
    fn current_ticks() -> u64;

    /// Converts hardware ticks to nanoseconds.
    fn ticks_to_nanos(ticks: u64) -> u64;

    /// Samples the raw scheduler clock directly in nanoseconds.
    ///
    /// The platform must read and convert one counter sample within this
    /// operation. Scheduler clock correction is applied by `ax-plat` after
    /// this raw sample crosses the platform boundary.
    fn scheduler_clock_raw_nanos() -> u64;

    /// Converts nanoseconds to hardware ticks.
    fn nanos_to_ticks(nanos: u64) -> u64;

    /// Reports whether the current architecture counter is synchronized
    /// across every runtime CPU.
    fn scheduler_clock_stability() -> SchedulerClockStability;

    /// Return epoch offset in nanoseconds (wall time offset to monotonic
    /// clock start).
    fn epochoffset_nanos() -> u64;

    /// Returns the IRQ number for the timer interrupt.
    fn irq_num() -> irq_framework::IrqId;

    /// Set a one-shot timer.
    ///
    /// A timer interrupt will be triggered at the specified monotonic time
    /// deadline (in nanoseconds). This capability is infallible: an already
    /// elapsed or sub-resolution deadline must be clamped to the device's
    /// minimum non-zero delta before the method returns. Implementations must
    /// not silently leave the previous event armed.
    fn set_oneshot_timer(deadline_ns: u64);

    /// Returns whether a claimed timer IRQ must physically quiesce the
    /// one-shot source before the interrupt controller completes the edge.
    ///
    /// Edge-triggered or rearm-cleared devices return `false`; level-triggered
    /// devices whose expired comparator remains observable return `true`.
    fn oneshot_timer_requires_irq_quiesce() -> bool;

    /// Returns a stopped one-shot timer to its active state and programs it.
    ///
    /// The implementation owns the architecture-specific activation order.
    /// Edge devices may need to unmask before programming a minimum delta;
    /// level devices may need to replace an expired comparator before unmask
    /// so controller EOI cannot latch the old level again.
    fn resume_oneshot_timer(deadline_ns: u64);

    /// Stops the current CPU's one-shot timer until it is programmed again.
    ///
    /// The interrupt source must become unobservable and its comparator must
    /// be discarded so a later resume cannot inherit a stale event.
    fn cancel_oneshot_timer();
}

/// Initializes the current CPU's scheduler-clock anchor before scheduler use.
///
/// # Errors
///
/// Returns an error if `cpu_id` does not identify the current installed CPU
/// area or if that CPU clock is already online.
///
/// # Safety
///
/// The current CPU must be offline, non-migrating and unable to take an
/// interrupt that can access scheduler-clock state.
pub unsafe fn init_scheduler_clock(cpu_id: usize) -> Result<(), SchedulerClockError> {
    let stability = scheduler_clock_stability();
    let raw_clock = scheduler_clock_raw_nanos();
    // SAFETY: forwarded from this function's offline-CPU contract.
    unsafe { crate::scheduler_clock::online_current_cpu(cpu_id, raw_clock, stability) }
}

/// Stops the current CPU's scheduler-clock publication.
///
/// # Errors
///
/// Returns an error if `cpu_id` is not current or its clock is already offline.
///
/// # Safety
///
/// The scheduler must have closed remote admission to this CPU and the caller
/// must exclude migration, local IRQs and scheduler-clock re-entry.
pub unsafe fn shutdown_scheduler_clock(cpu_id: usize) -> Result<(), SchedulerClockError> {
    // SAFETY: forwarded from this function's scheduler lifecycle contract.
    unsafe { crate::scheduler_clock::offline_current_cpu(cpu_id) }
}

/// Samples the current CPU's comparable wrapping scheduler clock in nanoseconds.
///
/// Stable platforms use the synchronized system counter. Unstable platforms
/// update the current CPU's corrected local publication.
///
/// # Errors
///
/// Returns an error when an unstable current CPU clock has no available
/// CPU-local state.
///
/// # Safety
///
/// The caller must own an initialized scheduler CPU and prevent migration for
/// the complete operation. Scheduler callers satisfy this through the owner
/// runqueue IRQ-save lock.
#[inline]
pub unsafe fn scheduler_clock_source() -> Result<u64, SchedulerClockError> {
    let raw_clock = scheduler_clock_raw_nanos();
    // SAFETY: forwarded from this function's migration-exclusion contract.
    unsafe { crate::scheduler_clock::source_current(raw_clock) }
}

/// Samples the current CPU's scheduler clock before an outer hard interrupt.
///
/// This is the only runtime boundary allowed to move a scheduler clock from
/// the stable fast path to corrected per-CPU clocks. The transition therefore
/// cannot split one hard-interrupt accounting interval across two clock
/// epochs.
///
/// # Errors
///
/// Returns an error if the current CPU clock has not been initialized.
///
/// # Safety
///
/// The caller must exclude migration and local IRQ re-entry, and must invoke
/// this function before starting the outer hard-interrupt time interval.
#[inline]
pub unsafe fn scheduler_clock_hardirq_sample() -> Result<u64, SchedulerClockError> {
    let stability = scheduler_clock_stability();
    let raw_clock = scheduler_clock_raw_nanos();
    // SAFETY: forwarded from this function's outer hard-IRQ entry contract.
    unsafe { crate::scheduler_clock::hardirq_sample(raw_clock, stability) }
}

/// Stamps the current CPU's scheduler clock from a local timer interrupt.
///
/// Clock stability transitions are deliberately excluded from this API. They
/// are committed before outer hard-interrupt accounting begins.
///
/// # Errors
///
/// Returns an error if the current CPU clock has not been initialized.
///
/// # Safety
///
/// The caller must exclude migration and local scheduler-clock re-entry. The
/// local timer interrupt path naturally satisfies both conditions.
#[inline]
pub unsafe fn scheduler_clock_tick() -> Result<u64, SchedulerClockError> {
    let raw_clock = scheduler_clock_raw_nanos();
    // SAFETY: forwarded from this function's local tick contract.
    unsafe { crate::scheduler_clock::tick(raw_clock) }
}

/// Returns nanoseconds elapsed since system boot.
pub fn monotonic_time_nanos() -> u64 {
    ticks_to_nanos(current_ticks())
}

/// Returns the time elapsed since system boot in [`TimeValue`].
pub fn monotonic_time() -> TimeValue {
    TimeValue::from_nanos(monotonic_time_nanos())
}

/// Returns nanoseconds elapsed since epoch (also known as realtime).
pub fn wall_time_nanos() -> u64 {
    adjusted_wall_time_nanos(
        base_wall_time_nanos(),
        WALL_TIME_ADJUSTMENT_NANOS.load(Ordering::Acquire),
    )
}

/// Returns the time elapsed since epoch (also known as realtime) in [`TimeValue`].
pub fn wall_time() -> TimeValue {
    TimeValue::from_nanos(wall_time_nanos())
}

/// Sets the system-wide wall clock without changing the monotonic clock.
///
/// The platform epoch remains the boot-time reference. This function stores a
/// signed adjustment relative to that reference so every wall-clock consumer
/// observes the same value while scheduler and relative-time accounting remain
/// tied to the monotonic counter.
///
/// # Errors
///
/// Returns [`WallTimeError::BeforeMonotonic`] if `new_time` is earlier than
/// the current monotonic time. Returns
/// [`WallTimeError::AdjustmentOutOfRange`] if either the timestamp or its
/// adjustment cannot be represented by the shared clock state.
pub fn set_wall_time(new_time: TimeValue) -> Result<(), WallTimeError> {
    let monotonic_nanos = monotonic_time_nanos();
    let requested_nanos =
        u64::try_from(new_time.as_nanos()).map_err(|_| WallTimeError::AdjustmentOutOfRange)?;
    // Match Linux do_settimeofday64 after its timespec validation:
    // wall_to_monotonic = monotonic - old_realtime, so rejecting
    // wall_to_monotonic > new_realtime - old_realtime rejects exactly
    // new_realtime < monotonic. clock_settime(2) documents this since Linux 4.3.
    if requested_nanos < monotonic_nanos {
        return Err(WallTimeError::BeforeMonotonic);
    }

    let base_nanos = monotonic_nanos.saturating_add(epochoffset_nanos());
    let adjustment = i128::from(requested_nanos) - i128::from(base_nanos);
    let adjustment = i64::try_from(adjustment).map_err(|_| WallTimeError::AdjustmentOutOfRange)?;
    WALL_TIME_ADJUSTMENT_NANOS.store(adjustment, Ordering::Release);
    Ok(())
}

fn base_wall_time_nanos() -> u64 {
    monotonic_time_nanos().saturating_add(epochoffset_nanos())
}

fn adjusted_wall_time_nanos(base_nanos: u64, adjustment_nanos: i64) -> u64 {
    if adjustment_nanos >= 0 {
        base_nanos.saturating_add(adjustment_nanos as u64)
    } else {
        base_nanos.saturating_sub(adjustment_nanos.unsigned_abs())
    }
}

/// Busy waiting for the given duration.
pub fn busy_wait(dur: Duration) {
    busy_wait_until(monotonic_time() + dur);
}

/// Busy waiting until reaching the given monotonic deadline.
pub fn busy_wait_until(deadline: TimeValue) {
    while monotonic_time() < deadline {
        core::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_time_adjustment_moves_forward_and_backward() {
        assert_eq!(adjusted_wall_time_nanos(20, 5), 25);
        assert_eq!(adjusted_wall_time_nanos(20, -5), 15);
    }

    #[test]
    fn wall_time_adjustment_saturates_at_clock_bounds() {
        assert_eq!(adjusted_wall_time_nanos(u64::MAX - 1, 5), u64::MAX);
        assert_eq!(adjusted_wall_time_nanos(1, -5), 0);
    }
}
