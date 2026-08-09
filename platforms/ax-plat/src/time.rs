//! Time-related operations.

pub use core::time::Duration;

/// A measurement of the system clock.
///
/// Currently, it reuses the [`core::time::Duration`] type. But it does not
/// represent a duration, but a clock time.
pub type TimeValue = Duration;

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

/// Time-related interfaces.
#[def_plat_interface]
pub trait TimeIf {
    /// Returns the current clock time in hardware ticks.
    fn current_ticks() -> u64;

    /// Converts hardware ticks to nanoseconds.
    fn ticks_to_nanos(ticks: u64) -> u64;

    /// Converts nanoseconds to hardware ticks.
    fn nanos_to_ticks(nanos: u64) -> u64;

    /// Reports whether the current architecture counter is synchronized
    /// across every runtime CPU.
    fn scheduler_clock_stability() -> SchedulerClockStability;

    /// Return epoch offset in nanoseconds (wall time offset to monotonic
    /// clock start).
    fn epochoffset_nanos() -> u64;

    /// Returns the IRQ number for the timer interrupt.
    #[cfg(feature = "irq")]
    fn irq_num() -> irq_framework::IrqId;

    /// Set a one-shot timer.
    ///
    /// A timer interrupt will be triggered at the specified monotonic time
    /// deadline (in nanoseconds).
    #[cfg(feature = "irq")]
    fn set_oneshot_timer(deadline_ns: u64);
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
    let raw_clock = ticks_to_nanos(current_ticks());
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

/// Samples `cpu_id`'s comparable wrapping scheduler clock in nanoseconds.
///
/// Stable platforms use the calling CPU's synchronized system counter.
/// Unstable platforms update the calling CPU's local publication, then couple
/// it atomically with the target publication without reading the target raw
/// counter.
///
/// # Errors
///
/// Returns an error when the target or calling CPU clock is offline, or when
/// `cpu_id` is outside the installed CPU-local layout.
///
/// # Safety
///
/// The caller must prevent migration for the complete operation. Scheduler
/// callers normally satisfy this through the target runqueue IRQ-save lock.
#[inline]
pub unsafe fn scheduler_clock_source(cpu_id: usize) -> Result<u64, SchedulerClockError> {
    let raw_clock = ticks_to_nanos(current_ticks());
    // SAFETY: forwarded from this function's migration-exclusion contract.
    unsafe { crate::scheduler_clock::source(cpu_id, raw_clock) }
}

/// Stamps the current CPU's scheduler clock from a local timer interrupt.
///
/// The stability assessment is refreshed here so a late x86 TSC adjustment
/// can move the owner from the direct fast path to corrected per-CPU clocks
/// without a discontinuity.
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
    let stability = scheduler_clock_stability();
    let raw_clock = ticks_to_nanos(current_ticks());
    // SAFETY: forwarded from this function's local tick contract.
    unsafe { crate::scheduler_clock::tick(raw_clock, stability) }
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
    monotonic_time_nanos() + epochoffset_nanos()
}

/// Returns the time elapsed since epoch (also known as realtime) in [`TimeValue`].
pub fn wall_time() -> TimeValue {
    TimeValue::from_nanos(monotonic_time_nanos() + epochoffset_nanos())
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
