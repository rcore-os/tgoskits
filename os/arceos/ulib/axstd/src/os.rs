//! OS-specific functionality.

/// ArceOS-specific definitions.
///
/// `api` re-exports the public ArceOS API surface. Prefer this entry for
/// ArceOS-specific operations that do not have a std-like wrapper.
///
/// `modules` re-exports lower-level ArceOS modules as an escape hatch for
/// complex systems such as Axvisor. Ordinary applications should prefer
/// `ax_std::{fs, io, thread, sync, time, net}`.
pub mod arceos {
    /// ArceOS public API facade.
    pub use ax_api as api;

    /// Guards for ArceOS interrupt and preemption contexts.
    pub mod guard {
        pub use ax_kernel_guard::{IrqSave, NoOp, NoPreempt, NoPreemptIrqSave};
    }

    /// Lower-level ArceOS module facade for system components.
    #[doc(no_inline)]
    pub use ax_api::modules;
    /// ArceOS host driver registry and firmware discovery capabilities.
    #[doc(no_inline)]
    pub use ax_driver as driver;
    /// ArceOS per-CPU storage and CPU-pinning capabilities.
    #[doc(no_inline)]
    pub use ax_percpu as percpu;

    /// Non-sleeping synchronization for ArceOS kernel contexts.
    pub mod sync {
        /// A mutex that disables preemption and local interrupts while held.
        pub type IrqSafeMutex<T> = ax_kspin::SpinNoIrq<T>;
        /// A guard returned by [`IrqSafeMutex::lock`].
        pub type IrqSafeMutexGuard<'a, T> = ax_kspin::SpinNoIrqGuard<'a, T>;

        /// A mutex that disables preemption while held.
        ///
        /// Callers must ensure the lock is not used by an interrupt handler.
        pub type NoPreemptMutex<T> = ax_kspin::SpinNoPreempt<T>;
        /// A guard returned by [`NoPreemptMutex::lock`].
        pub type NoPreemptMutexGuard<'a, T> = ax_kspin::SpinNoPreemptGuard<'a, T>;

        /// A raw spin lock that does not alter interrupt or preemption state.
        ///
        /// Callers must disable preemption and local interrupts before taking
        /// this lock, or prove that interrupt handlers never acquire it.
        pub type RawSpinLock<T> = ax_kspin::SpinRaw<T>;
        /// A guard returned by [`RawSpinLock::lock`].
        pub type RawSpinLockGuard<'a, T> = ax_kspin::SpinRawGuard<'a, T>;
    }
}

#[cfg(feature = "std-compat")]
pub mod libc_compat;

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use super::arceos::sync::{IrqSafeMutex, NoPreemptMutex, RawSpinLock};

    static IRQ_SAFE: IrqSafeMutex<usize> = IrqSafeMutex::new(0);
    static NO_PREEMPT: NoPreemptMutex<usize> = NoPreemptMutex::new(0);
    static RAW: RawSpinLock<usize> = RawSpinLock::new(0);

    #[test]
    fn special_locks_support_const_initialization_and_try_lock() {
        *IRQ_SAFE.lock() += 1;
        *NO_PREEMPT.lock() += 1;
        *RAW.lock() += 1;

        assert_eq!(*IRQ_SAFE.try_lock().unwrap(), 1);
        assert_eq!(*NO_PREEMPT.try_lock().unwrap(), 1);
        assert_eq!(*RAW.try_lock().unwrap(), 1);
    }
}
