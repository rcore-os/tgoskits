//! Scheduler-owned synchronization facade and runtime bridge.
//!
//! The stable lock surface is collected in [`api`]. Runtime providers use
//! [`bridge`] for scheduler-owned PI, blocking, and lockdep capabilities. The
//! bridge never owns a second waiter, donation graph, or wakeup state.

pub mod api;
#[doc(hidden)]
pub mod bridge;
mod context;

pub use self::api::*;
