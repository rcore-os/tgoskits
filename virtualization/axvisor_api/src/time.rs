// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Host time APIs for the AxVisor hypervisor.
//!
//! This module provides host monotonic time measurement and host timer
//! programming APIs, which are essential for implementing virtual timers and
//! time-related virtualization features.
//!
//! # Overview
//!
//! The time APIs provide:
//! - Current monotonic time queries
//! - Host one-shot timer programming
//!
//! # Types
//!
//! - [`TimeValue`] - A time value represented as [`Duration`].
//! - [`Nanos`] - Nanoseconds count (u64).
//! # Helper Functions
//!
//! In addition to the core API trait, this module provides helper functions:
//! - [`current_time`] - Get the current time as a [`TimeValue`].
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
//!     fn current_time_nanos() -> Nanos {
//!         // Read the host monotonic clock
//!     }
//!     // ... implement other functions
//! }
//! ```

use core::time::Duration;

/// Time value type.
///
/// Represents a point in time or a duration as a [`Duration`].
pub type TimeValue = Duration;

/// Nanoseconds count type.
///
/// Used for high-precision time measurements in nanoseconds.
pub type Nanos = u64;

/// The API trait for host time functionalities.
///
/// This trait defines the host time interface required by the hypervisor.
/// Implementations should be provided by the host system or HAL layer.
#[crate::api_def]
pub trait TimeIf {
    /// Get the current host monotonic time in nanoseconds.
    fn current_time_nanos() -> Nanos;

    /// Program the host one-shot timer to fire at `deadline`.
    ///
    /// The deadline is expressed in the same monotonic time domain as
    /// [`current_time_nanos`].
    fn set_oneshot_timer(deadline: TimeValue);
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
