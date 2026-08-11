//! Stable synchronization API exported by the scheduler layer.
//!
//! The concrete lock algorithms are migrated into this crate in independent
//! stages. During that migration these exports preserve the current PI mutex
//! and execution-context semantics rather than substituting the simpler
//! wait-queue mutex from the layering reference.

pub use ax_sync::{
    InterruptibleMutexExt, LockSubclass, LockdepMutexExt, Mutex, MutexGuard, PiMutex, PiMutexGuard,
    PiMutexLockInterrupted, RawMutex, RawPiMutex, dump_lockdep_trace, set_lockdep_trace_enabled,
};

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
