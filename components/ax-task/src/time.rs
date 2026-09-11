//! Monotonic time and owner-CPU timer callbacks.

pub use crate::runtime::clock::{KTIME_MAX_NANOS, MonotonicDeadline, MonotonicInstant};

pub mod timer;

pub mod hard_timer;

pub(crate) mod registration;

pub(crate) mod queue;
