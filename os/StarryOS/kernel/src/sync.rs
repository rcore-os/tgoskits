//! StarryOS synchronization facade.
//!
//! Kernel code imports locks only through this module so the type name and
//! acquisition operation preserve sleep, preemption, and IRQ semantics.

pub(crate) use ax_runtime::sync::{
    InterruptibleMutexExt, IrqMutex, LockdepMutexExt, Mutex, PiMutex, PiMutexGuard,
    PreemptGuard as NoPreempt, PreemptIrqSaveGuard as NoPreemptIrqSave, SpinLock,
    SpinLock as NoPreemptMutex, SpinLockGuard, SpinRwLock as RwLock,
};
