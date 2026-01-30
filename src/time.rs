//! Time and timer APIs for the AxVisor hypervisor.
//!
//! This module provides APIs for time measurement and timer management,
//! which are essential for implementing virtual timers and time-related
//! virtualization features.
//!
//! # Overview
//!
//! The time APIs provide:
//! - Current time and tick count queries
//! - Conversion between ticks, nanoseconds, and duration
//! - Timer registration and cancellation
//!
//! # Types
//!
//! - [`TimeValue`] - A time value represented as [`Duration`].
//! - [`Nanos`] - Nanoseconds count (u64).
//! - [`Ticks`] - Tick count (u64).
//! - [`CancelToken`] - Token used to cancel a registered timer.
//!
//! # Helper Functions
//!
//! In addition to the core API trait, this module provides helper functions:
//! - [`current_time_nanos`] - Get the current time in nanoseconds.
//! - [`current_time`] - Get the current time as a [`TimeValue`].
//! - [`ticks_to_time`] - Convert ticks to [`TimeValue`].
//! - [`time_to_ticks`] - Convert [`TimeValue`] to ticks.
//!
//! # Implementation
//!
//! To implement these APIs, use the [`api_impl`](crate::api_impl) attribute
//! macro on an impl block:
//!
//! ```rust,ignore
//! struct TimeIfImpl;
//!
//! #[axvisor_api::api_impl]
//! impl axvisor_api::time::TimeIf for TimeIfImpl {
//!     fn current_ticks() -> Ticks {
//!         // Read the hardware timer counter
//!     }
//!     // ... implement other functions
//! }
//! ```

extern crate alloc;

use alloc::boxed::Box;
use core::time::Duration;

/// Time value type.
///
/// Represents a point in time or a duration as a [`Duration`].
pub type TimeValue = Duration;

/// Nanoseconds count type.
///
/// Used for high-precision time measurements in nanoseconds.
pub type Nanos = u64;

/// Tick count type.
///
/// Represents the raw hardware timer counter value.
pub type Ticks = u64;

/// Cancel token type for timer cancellation.
///
/// This token is returned when registering a timer and can be used to cancel
/// the timer before it fires.
pub type CancelToken = usize;

/// The API trait for time and timer functionalities.
///
/// This trait defines the core time management interface required by the
/// hypervisor. Implementations should be provided by the host system or HAL
/// layer.
#[crate::api_def]
pub trait TimeIf {
    /// Get the current tick count from the hardware timer.
    ///
    /// The tick count is a monotonically increasing counter that can be
    /// converted to time using [`ticks_to_nanos`].
    ///
    /// # Returns
    ///
    /// The current hardware timer counter value.
    fn current_ticks() -> Ticks;

    /// Convert a tick count to nanoseconds.
    ///
    /// # Arguments
    ///
    /// * `ticks` - The tick count to convert.
    ///
    /// # Returns
    ///
    /// The equivalent time in nanoseconds.
    fn ticks_to_nanos(ticks: Ticks) -> Nanos;

    /// Convert nanoseconds to a tick count.
    ///
    /// # Arguments
    ///
    /// * `nanos` - The nanoseconds to convert.
    ///
    /// # Returns
    ///
    /// The equivalent tick count.
    fn nanos_to_ticks(nanos: Nanos) -> Ticks;

    /// Register a timer that will fire at the specified deadline.
    ///
    /// When the deadline is reached, the callback function will be called
    /// with the actual time at which it was invoked.
    ///
    /// # Arguments
    ///
    /// * `deadline` - The time at which the timer should fire.
    /// * `callback` - The function to call when the timer fires. It receives
    ///   the actual time as an argument.
    ///
    /// # Returns
    ///
    /// A [`CancelToken`] that can be used to cancel the timer with
    /// [`cancel_timer`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use axvisor_api::time::{register_timer, current_time, TimeValue};
    /// use core::time::Duration;
    ///
    /// let deadline = current_time() + Duration::from_millis(100);
    /// let token = register_timer(deadline, Box::new(|actual_time| {
    ///     println!("Timer fired at {:?}", actual_time);
    /// }));
    /// ```
    fn register_timer(
        deadline: TimeValue,
        callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
    ) -> CancelToken;

    /// Cancel a previously registered timer.
    ///
    /// If the timer has already fired, this function has no effect.
    ///
    /// # Arguments
    ///
    /// * `token` - The cancel token returned by [`register_timer`].
    fn cancel_timer(token: CancelToken);
}

/// Get the current time in nanoseconds.
///
/// This is a convenience function that combines [`current_ticks`] and
/// [`ticks_to_nanos`].
///
/// # Returns
///
/// The current time in nanoseconds since an unspecified epoch.
pub fn current_time_nanos() -> Nanos {
    ticks_to_nanos(current_ticks())
}

/// Get the current time as a [`TimeValue`].
///
/// This is a convenience function that returns the current time as a
/// [`Duration`].
///
/// # Returns
///
/// The current time as a [`TimeValue`] (Duration).
pub fn current_time() -> TimeValue {
    Duration::from_nanos(current_time_nanos())
}

/// Convert ticks to a [`TimeValue`].
///
/// # Arguments
///
/// * `ticks` - The tick count to convert.
///
/// # Returns
///
/// The equivalent time as a [`TimeValue`] (Duration).
pub fn ticks_to_time(ticks: Ticks) -> TimeValue {
    Duration::from_nanos(ticks_to_nanos(ticks))
}

/// Convert a [`TimeValue`] to ticks.
///
/// # Arguments
///
/// * `time` - The time value to convert.
///
/// # Returns
///
/// The equivalent tick count.
pub fn time_to_ticks(time: TimeValue) -> Ticks {
    nanos_to_ticks(time.as_nanos() as Nanos)
}
