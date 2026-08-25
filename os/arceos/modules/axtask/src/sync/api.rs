//! Stable ArceOS synchronization API.

#[cfg(feature = "lockdep")]
pub use super::lockdep::*;
pub use super::{context::*, mutex::*, spin::*};
#[cfg(not(feature = "lockdep"))]
pub use super::{dump_lockdep_trace, set_lockdep_trace_enabled};
