//! Stable ArceOS synchronization API.

#[cfg(feature = "lockdep")]
pub use super::lockdep::*;
#[cfg(feature = "multitask")]
pub use super::mutex::*;
pub use super::{context::*, spin::*};
#[cfg(not(feature = "lockdep"))]
pub use super::{dump_lockdep_trace, set_lockdep_trace_enabled};
