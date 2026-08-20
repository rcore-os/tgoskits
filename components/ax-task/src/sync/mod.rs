//! Scheduler-owned synchronization facade and runtime bridge.
//!
//! The stable lock surface is collected in [`api`]. Runtime providers use
//! [`bridge`] for scheduler-owned PI, blocking, and lockdep capabilities. The
//! bridge never owns a second waiter, donation graph, or wakeup state.

pub mod api;
#[doc(hidden)]
pub mod bridge;
mod context;
#[cfg(any(feature = "lockdep", all(axtest, feature = "axtest")))]
pub(crate) mod lockdep;
mod mutex;
mod spin;

pub use self::api::*;
#[doc(hidden)]
pub use self::mutex::{
    PI_MUTEX_WAIT_STORAGE_WORDS, PiMutexAcquire, PiMutexClaimOutcome, PiMutexCore, PiMutexCoreView,
    PiMutexId, PiMutexLockResult, PiMutexOwnedRelease, PiMutexOwnerSnapshot, PiMutexRaw,
    PiMutexRef, PiMutexStateError, PiMutexWaitStorageView, PiTaskId, PiWaitCancelOutcome,
    PiWaitToken,
};
