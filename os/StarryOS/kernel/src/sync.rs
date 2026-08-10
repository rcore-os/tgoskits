//! StarryOS synchronization facade.
//!
//! Kernel code imports locks only through this module so the type name and
//! acquisition operation preserve sleep, preemption, and IRQ semantics.

pub(crate) use ax_sync::{
    LockdepMutexExt, Mutex, MutexGuard, PreemptIrqSaveGuard, RawIrqSaveMutex as RawSpinNoIrq,
    SpinLock, SpinLock as NoPreemptMutex, SpinRwLock as RwLock,
};
pub(crate) use axnsproxy::IrqMutex;

/// A read-write lock whose read ownership may span a scheduler context switch.
///
/// Ordinary users get the same preemption-safe semantics as [`RwLock`]. The
/// raw pair is reserved for `TaskExt::{on_enter,on_leave}`, where the scheduler
/// already keeps IRQs and preemption disabled and the read reference is
/// installed into `scope-local` until that task is switched out.
pub(crate) struct ContextSwitchRwLock<T: ?Sized>(ax_sync::SpinRwLock<T>);

impl<T> ContextSwitchRwLock<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(ax_sync::SpinRwLock::new(value))
    }
}

impl<T: ?Sized> ContextSwitchRwLock<T> {
    #[track_caller]
    pub(crate) fn read(&self) -> ax_sync::SpinRwLockReadGuard<'_, T> {
        self.0.read()
    }

    #[track_caller]
    pub(crate) fn write(&self) -> ax_sync::SpinRwLockWriteGuard<'_, T> {
        self.0.write()
    }

    /// Acquires the read side for a task's active `scope-local` installation.
    ///
    /// # Safety
    ///
    /// The scheduler or caller must prevent migration, preemption, and local
    /// IRQ re-entry until the returned guard is either dropped or deliberately
    /// forgotten and paired with [`Self::release_context_switch_reader`].
    #[track_caller]
    pub(crate) unsafe fn read_for_context_switch(&self) -> ax_sync::RawSpinRwLockReadGuard<'_, T> {
        unsafe { self.0.read_raw() }
    }

    /// Releases a deliberately forgotten context-switch read guard.
    ///
    /// # Safety
    ///
    /// The caller must own exactly one forgotten guard returned by
    /// [`Self::read_for_context_switch`], must have removed every reference
    /// derived from it, and must prevent concurrent lifecycle operations.
    pub(crate) unsafe fn release_context_switch_reader(&self) {
        unsafe { self.0.force_read_decrement_raw() };
    }
}
