//! Time and timer APIs.

extern crate alloc;

use alloc::boxed::Box;
use core::time::Duration;

/// Time value.
pub type TimeValue = Duration;
/// Nanoseconds count.
pub type Nanos = u64;
/// Tick count.
pub type Ticks = u64;
/// Cancel token, used to cancel a scheduled timer event.
pub type CancelToken = usize;

/// The API trait for time and timer functionalities.
#[crate::api_def]
pub trait TimeIf {
    /// Get the current tick count.
    fn current_ticks() -> Ticks;
    /// Convert ticks to nanoseconds.
    fn ticks_to_nanos(ticks: Ticks) -> Nanos;
    /// Convert nanoseconds to ticks.
    fn nanos_to_ticks(nanos: Nanos) -> Ticks;
    /// Register a timer.
    fn register_timer(
        deadline: TimeValue,
        callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
    ) -> CancelToken;
    /// Cancel a timer.
    fn cancel_timer(token: CancelToken);
}

/// Get the current time in nanoseconds.
pub fn current_time_nanos() -> Nanos {
    ticks_to_nanos(current_ticks())
}

/// Get the current time.
pub fn current_time() -> TimeValue {
    Duration::from_nanos(current_time_nanos())
}

/// Convert ticks to time.
pub fn ticks_to_time(ticks: Ticks) -> TimeValue {
    Duration::from_nanos(ticks_to_nanos(ticks))
}

/// Convert time to ticks.
pub fn time_to_ticks(time: TimeValue) -> Ticks {
    nanos_to_ticks(time.as_nanos() as Nanos)
}
