//! StarryOS synchronization facade.
//!
//! Kernel code imports locks only through this module so the type name and
//! acquisition operation preserve sleep, preemption, and IRQ semantics.

pub(crate) use ax_sync::{
    LockdepMutexExt, Mutex, MutexGuard, PreemptIrqSaveGuard, RawIrqSaveMutex as RawSpinNoIrq,
    SpinLock as NoPreemptMutex, SpinLockGuard as NoPreemptMutexGuard, SpinRwLock as RwLock,
    SpinRwLockReadGuard as RwLockReadGuard, SpinRwLockWriteGuard as RwLockWriteGuard,
};
pub(crate) use axnsproxy::{IrqMutex, IrqMutexGuard};
