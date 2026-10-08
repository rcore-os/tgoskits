//! Filesystem synchronization facades.
//!
//! Production code uses the real primitives directly: the sleepable
//! [`Mutex`] for blocking paths and [`RawSpinLock`] with an explicit
//! IRQ-save acquisition (`lock_irqsave`, `try_lock_irqsave`,
//! `lock_irqsave_nested`) for non-sleeping paths. Host tests substitute shims
//! that keep the same public names and acquisition methods while tracking
//! whether the current thread already holds a non-sleeping lock.

#[cfg(not(test))]
pub use ax_sync::{Mutex, MutexGuard, RawSpinLock, RawSpinLockIrqSaveGuard};
#[cfg(test)]
pub use tests::{Mutex, MutexGuard, RawSpinLock, RawSpinLockIrqSaveGuard};

#[cfg(test)]
pub(crate) fn current_thread_holds_irq_mutex() -> bool {
    tests::current_thread_holds_irq_mutex()
}

#[cfg(test)]
mod tests {
    use core::{
        cell::Cell,
        fmt,
        ops::{Deref, DerefMut},
    };
    use std::sync::TryLockError;

    use ax_sync::{
        RawSpinLock as ProductionRawSpinLock,
        RawSpinLockIrqSaveGuard as ProductionRawSpinLockIrqSaveGuard,
    };

    std::thread_local! {
        static IRQ_MUTEX_DEPTH: Cell<usize> = const { Cell::new(0) };
    }

    pub struct RawSpinLock<T: ?Sized>(ProductionRawSpinLock<T>);

    pub struct RawSpinLockIrqSaveGuard<'a, T: ?Sized> {
        inner: Option<ProductionRawSpinLockIrqSaveGuard<'a, T>>,
    }

    pub struct Mutex<T: ?Sized>(std::sync::Mutex<T>);

    pub struct MutexGuard<'a, T: ?Sized>(std::sync::MutexGuard<'a, T>);

    pub(super) fn current_thread_holds_irq_mutex() -> bool {
        IRQ_MUTEX_DEPTH.with(Cell::get) != 0
    }

    impl<T> RawSpinLock<T> {
        #[track_caller]
        pub const fn new(value: T) -> Self {
            Self(ProductionRawSpinLock::new(value))
        }

        pub fn into_inner(self) -> T {
            self.0.into_inner()
        }
    }

    impl<T: Default> Default for RawSpinLock<T> {
        fn default() -> Self {
            Self::new(T::default())
        }
    }

    impl<T: ?Sized> RawSpinLock<T> {
        #[track_caller]
        pub fn lock_irqsave(&self) -> RawSpinLockIrqSaveGuard<'_, T> {
            let inner = self.0.lock_irqsave();
            IRQ_MUTEX_DEPTH.with(|depth| depth.set(depth.get() + 1));
            RawSpinLockIrqSaveGuard { inner: Some(inner) }
        }

        #[track_caller]
        pub fn lock_irqsave_nested(&self, subclass: u32) -> RawSpinLockIrqSaveGuard<'_, T> {
            let inner = self.0.lock_irqsave_nested(subclass);
            IRQ_MUTEX_DEPTH.with(|depth| depth.set(depth.get() + 1));
            RawSpinLockIrqSaveGuard { inner: Some(inner) }
        }

        #[track_caller]
        pub fn try_lock_irqsave(&self) -> Option<RawSpinLockIrqSaveGuard<'_, T>> {
            self.0.try_lock_irqsave().map(|inner| {
                IRQ_MUTEX_DEPTH.with(|depth| depth.set(depth.get() + 1));
                RawSpinLockIrqSaveGuard { inner: Some(inner) }
            })
        }
    }

    impl<T: ?Sized> Deref for RawSpinLockIrqSaveGuard<'_, T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            self.inner.as_deref().expect("IRQ-save spin guard is live")
        }
    }

    impl<T: ?Sized> DerefMut for RawSpinLockIrqSaveGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.inner
                .as_deref_mut()
                .expect("IRQ-save spin guard is live")
        }
    }

    impl<T: ?Sized> Drop for RawSpinLockIrqSaveGuard<'_, T> {
        fn drop(&mut self) {
            drop(self.inner.take());
            IRQ_MUTEX_DEPTH.with(|depth| {
                let held = depth.get();
                assert!(held != 0, "IRQ-save spin ownership depth underflow");
                depth.set(held - 1);
            });
        }
    }

    impl<T: fmt::Debug + ?Sized> fmt::Debug for RawSpinLockIrqSaveGuard<'_, T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(&**self, f)
        }
    }

    impl<T> Mutex<T> {
        #[track_caller]
        pub const fn new(value: T) -> Self {
            Self(std::sync::Mutex::new(value))
        }

        pub fn into_inner(self) -> T {
            self.0.into_inner().unwrap_or_else(|err| err.into_inner())
        }
    }

    impl<T: Default> Default for Mutex<T> {
        fn default() -> Self {
            Self::new(T::default())
        }
    }

    impl<T: ?Sized> Mutex<T> {
        #[track_caller]
        pub fn lock(&self) -> MutexGuard<'_, T> {
            MutexGuard(self.0.lock().unwrap_or_else(|err| err.into_inner()))
        }

        #[track_caller]
        pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
            match self.0.try_lock() {
                Ok(guard) => Some(MutexGuard(guard)),
                Err(TryLockError::Poisoned(err)) => Some(MutexGuard(err.into_inner())),
                Err(TryLockError::WouldBlock) => None,
            }
        }
    }

    impl<T: ?Sized> Deref for MutexGuard<'_, T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    impl<T: fmt::Debug + ?Sized> fmt::Debug for MutexGuard<'_, T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(&**self, f)
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::{RawSpinLock, current_thread_holds_irq_mutex};

    #[test]
    fn irq_mutex_ownership_is_local_to_the_holding_thread() {
        let lock = RawSpinLock::new(());
        assert!(!current_thread_holds_irq_mutex());

        let guard = lock.lock_irqsave();
        assert!(current_thread_holds_irq_mutex());
        std::thread::scope(|scope| {
            scope.spawn(|| assert!(!current_thread_holds_irq_mutex()));
        });

        drop(guard);
        assert!(!current_thread_holds_irq_mutex());
    }
}
