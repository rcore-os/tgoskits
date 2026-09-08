//! Scheduler-owned synchronization facade and runtime bridge.
//!
//! The stable lock surface is collected in [`api`]. Runtime providers use
//! [`bridge`] for scheduler-owned PI, blocking, and lockdep capabilities. The
//! bridge never owns a second waiter, donation graph, or wakeup state.

#[doc(hidden)]
pub(crate) mod bridge;
mod context;
#[cfg(feature = "lockdep")]
pub(crate) mod lockdep;
pub(crate) mod mutex;
mod spin;

#[cfg(feature = "lockdep")]
pub use {
    self::lockdep::LockSubclass, self::lockdep::dump_lockdep_trace,
    self::lockdep::set_lockdep_trace_enabled,
};

pub use self::mutex::{
    InterruptibleMutexExt, LockdepMutexExt, Mutex, MutexGuard, PiMutexLockInterrupted, RawMutex,
};
#[cfg(not(feature = "lockdep"))]
pub type LockSubclass = u32;
#[cfg(not(feature = "lockdep"))]
pub const fn set_lockdep_trace_enabled(_enabled: bool) {}
#[cfg(not(feature = "lockdep"))]
pub const fn dump_lockdep_trace() {}

pub use self::context::{
    IrqReturnPreemptGuard, IrqSaveGuard, PreemptGuard, PreemptIrqSaveGuard, hardirq_enter,
    hardirq_exit,
};
pub use crate::sync::spin::{
    RawIrqSaveMutex, RawSpinLockGuard, RawSpinRwLockReadGuard, RawSpinRwLockWriteGuard, SpinLock,
    SpinLockGuard, SpinLockIrqSaveGuard, SpinRwLock, SpinRwLockIrqSaveReadGuard,
    SpinRwLockIrqSaveWriteGuard, SpinRwLockReadGuard, SpinRwLockWriteGuard,
};

/// A non-sleeping mutex whose guard saves and disables local IRQs.
pub type IrqMutex<T> = lock_api::Mutex<RawIrqSaveMutex, T>;

pub use crate::sync::wait_queue::{
    WaitQueue, WaitQueueRegistration, WaitQueueWakeOutcome, WaitQueueWakeToken,
    wait_until_registered,
};

pub mod irq;

pub mod membarrier;

pub(crate) mod wait_queue;
