//! StarryOS synchronization facade.
//!
//! Kernel code imports locks only through this module so the type name and
//! acquisition operation preserve sleep, preemption, and IRQ semantics.

pub(crate) use ax_fs_ng::os::sync::Mutex as FsMutex;
pub(crate) use ax_runtime::sync::{
    InterruptibleMutexExt, IrqMutex, LockdepMutexExt, Mutex, PiMutex, PiMutexGuard,
    PreemptIrqSaveGuard as NoPreemptIrqSave, RawIrqSaveMutex, SpinLock, SpinLockGuard, SpinRwLock,
};

pub(crate) type NoPreemptMutex<T> = SpinLock<T>;
pub(crate) type RawSpinNoIrq = RawIrqSaveMutex;
pub(crate) type RwLock<T> = SpinRwLock<T>;
