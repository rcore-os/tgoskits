//! StarryOS synchronization facade.
//!
//! Kernel code imports locks only through this module so the type name and
//! acquisition operation preserve sleep, preemption, and IRQ semantics.

pub(crate) use ax_fs_ng::os::sync::SleepMutex as FsMutex;
pub(crate) use ax_runtime::sync::{
    InterruptibleMutexExt, LockdepMutexExt, Mutex, PiMutex, PiMutexGuard, PreemptGuard,
    PreemptIrqSaveGuard as NoPreemptIrqSave, RawIrqSaveMutex, SpinLock, SpinLockGuard, SpinRwLock,
};

/// An IRQ-save spin mutex for state reachable from interrupt context.
#[repr(transparent)]
pub(crate) struct IrqMutex<T: ?Sized>(ax_runtime::sync::SpinLock<T>);

pub(crate) type IrqMutexGuard<'a, T> = ax_runtime::sync::SpinLockIrqSaveGuard<'a, T>;

impl<T> IrqMutex<T> {
    #[track_caller]
    pub(crate) const fn new(value: T) -> Self {
        Self(ax_runtime::sync::SpinLock::new(value))
    }

    #[track_caller]
    pub(crate) fn lock(&self) -> IrqMutexGuard<'_, T> {
        self.0.lock_irqsave()
    }

    /// Acquires this IRQ-save mutex using a lockdep subclass.
    #[track_caller]
    pub(crate) fn lock_nested(&self, subclass: u32) -> IrqMutexGuard<'_, T> {
        self.0.lock_irqsave_nested(subclass)
    }

    #[track_caller]
    pub(crate) fn try_lock(&self) -> Option<IrqMutexGuard<'_, T>> {
        self.0.try_lock_irqsave()
    }
}

impl<T: Default> Default for IrqMutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: core::fmt::Debug> core::fmt::Debug for IrqMutex<T> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(formatter)
    }
}

pub(crate) type NoPreemptMutex<T> = SpinLock<T>;
pub(crate) type RawSpinNoIrq = RawIrqSaveMutex;
pub(crate) type RwLock<T> = SpinRwLock<T>;
