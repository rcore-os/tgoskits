//! Stable synchronization API exported by the scheduler layer.
//!
//! These exports preserve the branch's urgency-ordered PI mutex and execution
//! context semantics; the external `ax-sync` wrappers use the same algorithms
//! through the runtime bridge.

#[cfg(feature = "lockdep")]
pub use super::lockdep::{LockSubclass, dump_lockdep_trace, set_lockdep_trace_enabled};
pub use super::mutex::{
    InterruptibleMutexExt, LockdepMutexExt, Mutex, MutexGuard, PiMutex, PiMutexGuard,
    PiMutexLockInterrupted, RawMutex, RawPiMutex,
};
#[cfg(not(feature = "lockdep"))]
pub type LockSubclass = u32;
#[cfg(not(feature = "lockdep"))]
pub const fn set_lockdep_trace_enabled(_enabled: bool) {}
#[cfg(not(feature = "lockdep"))]
pub const fn dump_lockdep_trace() {}

pub use super::context::{
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
