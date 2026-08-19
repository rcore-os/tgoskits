//! Native ArceOS synchronization implementation.
//!
//! This module owns the lock algorithms and their execution-context rules.
//! [`ax_runtime`](https://docs.rs/ax-runtime) re-exports the stable API for OS
//! consumers and adapts it to the OS-independent `ax-sync` bridge.

pub mod api;
#[doc(hidden)]
pub mod bridge;
mod context;
#[cfg(feature = "lockdep")]
mod lockdep;
#[cfg(feature = "multitask")]
mod mutex;
mod spin;

#[cfg(not(feature = "lockdep"))]
/// No-op trace switch for builds without lockdep.
pub const fn set_lockdep_trace_enabled(_enabled: bool) {}
#[cfg(not(feature = "lockdep"))]
/// No-op trace dump for builds without lockdep.
pub const fn dump_lockdep_trace() {}
pub use self::api::*;
