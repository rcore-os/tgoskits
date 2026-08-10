//! StarryOS synchronization facade.
//!
//! Kernel code imports locks only through this module so the type name and
//! acquisition operation preserve sleep, preemption, and IRQ semantics.

pub(crate) use ax_sync::{
    LockdepMutexExt, Mutex, MutexGuard, PreemptIrqSaveGuard, RawIrqSaveMutex as RawSpinNoIrq,
    SpinLock, SpinLock as NoPreemptMutex, SpinRwLock as RwLock,
};
pub(crate) use axnsproxy::IrqMutex;
