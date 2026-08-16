//! StarryOS synchronization facade.
//!
//! Kernel code imports locks only through this module so the type name and
//! acquisition operation preserve sleep, preemption, and IRQ semantics.

pub(crate) use ax_fs_ng::os::sync::SleepMutex as FsMutex;
pub(crate) use ax_runtime::sync::*;

/// An IRQ-save spin mutex for state reachable from interrupt context.
#[repr(transparent)]
pub struct IrqMutex<T: ?Sized>(ax_runtime::sync::SpinLock<T>);

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

/// A read-write lock whose read ownership may span a scheduler context switch.
///
/// Ordinary users get the same preemption-safe semantics as [`RwLock`]. The
/// raw pair is reserved for `TaskExt::{on_enter,on_leave}`, where the scheduler
/// already keeps IRQs and preemption disabled and the read reference is
/// installed into `scope-local` until that task is switched out.
pub(crate) struct ContextSwitchRwLock<T: ?Sized>(SpinRwLock<T>);

impl<T> ContextSwitchRwLock<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(SpinRwLock::new(value))
    }
}

impl<T: ?Sized> ContextSwitchRwLock<T> {
    #[track_caller]
    pub(crate) fn read(&self) -> SpinRwLockReadGuard<'_, T> {
        self.0.read()
    }

    #[track_caller]
    pub(crate) fn write(&self) -> SpinRwLockWriteGuard<'_, T> {
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
    pub(crate) unsafe fn read_for_context_switch(&self) -> RawSpinRwLockReadGuard<'_, T> {
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
