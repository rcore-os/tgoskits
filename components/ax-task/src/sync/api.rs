//! Stable synchronization API exported by the scheduler layer.
//!
//! The concrete lock algorithms are migrated into this crate in independent
//! stages. During that migration these exports preserve the current PI mutex
//! and execution-context semantics rather than substituting the simpler
//! wait-queue mutex from the layering reference.

pub use ax_sync::{
    InterruptibleMutexExt, LockdepMutexExt, Mutex, MutexGuard, PiMutex, PiMutexGuard,
    PiMutexLockInterrupted, RawMutex, RawPiMutex,
};

#[cfg(feature = "lockdep")]
pub use super::lockdep::{LockSubclass, dump_lockdep_trace, set_lockdep_trace_enabled};
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
    RawSpinLockGuard, SpinLock, SpinLockGuard, SpinLockIrqSaveGuard, SpinRwLock,
    SpinRwLockIrqSaveReadGuard, SpinRwLockIrqSaveWriteGuard, SpinRwLockReadGuard,
    SpinRwLockWriteGuard,
};

/// A non-sleeping mutex whose guard saves and disables local IRQs.
pub type IrqMutex<T> = lock_api::Mutex<super::spin::RawIrqSaveMutex, T>;
