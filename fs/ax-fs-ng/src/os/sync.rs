#[cfg(not(test))]
pub use ax_sync::{Mutex as SleepMutex, MutexGuard as SleepMutexGuard};
#[cfg(not(test))]
pub use production::{IrqMutex, IrqMutexGuard};

mod production {
    /// Filesystem-internal spin mutex for IRQ and completion paths.
    #[repr(transparent)]
    pub struct IrqMutex<T: ?Sized>(ax_sync::SpinLock<T>);

    pub type IrqMutexGuard<'a, T> = ax_sync::SpinLockIrqSaveGuard<'a, T>;

    impl<T> IrqMutex<T> {
        #[track_caller]
        pub const fn new(value: T) -> Self {
            Self(ax_sync::SpinLock::new(value))
        }

        pub fn into_inner(self) -> T {
            self.0.into_inner()
        }
    }

    impl<T: ?Sized> IrqMutex<T> {
        #[track_caller]
        pub fn lock(&self) -> IrqMutexGuard<'_, T> {
            self.0.lock_irqsave()
        }

        #[track_caller]
        pub fn try_lock(&self) -> Option<IrqMutexGuard<'_, T>> {
            self.0.try_lock_irqsave()
        }
    }

    impl<T: Default> Default for IrqMutex<T> {
        fn default() -> Self {
            Self::new(T::default())
        }
    }
}
#[cfg(test)]
pub use tests::{
    TestIrqMutex as IrqMutex, TestIrqMutexGuard as IrqMutexGuard, TestMutex as SleepMutex,
    TestMutexGuard as SleepMutexGuard,
};

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
    use std::sync::{Mutex, MutexGuard, TryLockError};

    use super::production;

    std::thread_local! {
        static IRQ_MUTEX_DEPTH: Cell<usize> = const { Cell::new(0) };
    }

    pub struct TestIrqMutex<T: ?Sized>(production::IrqMutex<T>);

    pub struct TestIrqMutexGuard<'a, T: ?Sized> {
        inner: Option<production::IrqMutexGuard<'a, T>>,
    }

    pub struct TestMutex<T: ?Sized>(Mutex<T>);

    pub struct TestMutexGuard<'a, T: ?Sized>(MutexGuard<'a, T>);

    pub(super) fn current_thread_holds_irq_mutex() -> bool {
        IRQ_MUTEX_DEPTH.with(|depth| depth.get() != 0)
    }

    impl<T> TestIrqMutex<T> {
        #[track_caller]
        pub const fn new(value: T) -> Self {
            Self(production::IrqMutex::new(value))
        }

        pub fn into_inner(self) -> T {
            self.0.into_inner()
        }
    }

    impl<T: Default> Default for TestIrqMutex<T> {
        fn default() -> Self {
            Self::new(T::default())
        }
    }

    impl<T: ?Sized> TestIrqMutex<T> {
        #[track_caller]
        pub fn lock(&self) -> TestIrqMutexGuard<'_, T> {
            let inner = self.0.lock();
            IRQ_MUTEX_DEPTH.with(|depth| depth.set(depth.get() + 1));
            TestIrqMutexGuard { inner: Some(inner) }
        }

        #[track_caller]
        pub fn try_lock(&self) -> Option<TestIrqMutexGuard<'_, T>> {
            self.0.try_lock().map(|inner| {
                IRQ_MUTEX_DEPTH.with(|depth| depth.set(depth.get() + 1));
                TestIrqMutexGuard { inner: Some(inner) }
            })
        }
    }

    impl<T: ?Sized> Deref for TestIrqMutexGuard<'_, T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            self.inner.as_deref().expect("IRQ mutex guard is live")
        }
    }

    impl<T: ?Sized> DerefMut for TestIrqMutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            self.inner.as_deref_mut().expect("IRQ mutex guard is live")
        }
    }

    impl<T: ?Sized> Drop for TestIrqMutexGuard<'_, T> {
        fn drop(&mut self) {
            drop(self.inner.take());
            IRQ_MUTEX_DEPTH.with(|depth| {
                let held = depth.get();
                assert!(held != 0, "IRQ mutex ownership depth underflow");
                depth.set(held - 1);
            });
        }
    }

    impl<T: fmt::Debug + ?Sized> fmt::Debug for TestIrqMutexGuard<'_, T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(&**self, f)
        }
    }

    impl<T> TestMutex<T> {
        #[track_caller]
        pub const fn new(value: T) -> Self {
            Self(Mutex::new(value))
        }

        pub fn into_inner(self) -> T {
            self.0.into_inner().unwrap_or_else(|err| err.into_inner())
        }
    }

    impl<T: Default> Default for TestMutex<T> {
        fn default() -> Self {
            Self::new(T::default())
        }
    }

    impl<T: ?Sized> TestMutex<T> {
        #[track_caller]
        pub fn lock(&self) -> TestMutexGuard<'_, T> {
            TestMutexGuard(self.0.lock().unwrap_or_else(|err| err.into_inner()))
        }

        #[track_caller]
        pub fn try_lock(&self) -> Option<TestMutexGuard<'_, T>> {
            match self.0.try_lock() {
                Ok(guard) => Some(TestMutexGuard(guard)),
                Err(TryLockError::Poisoned(err)) => Some(TestMutexGuard(err.into_inner())),
                Err(TryLockError::WouldBlock) => None,
            }
        }
    }

    impl<T: ?Sized> Deref for TestMutexGuard<'_, T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl<T: ?Sized> DerefMut for TestMutexGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    impl<T: fmt::Debug + ?Sized> fmt::Debug for TestMutexGuard<'_, T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Debug::fmt(&**self, f)
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::{IrqMutex, current_thread_holds_irq_mutex};

    #[test]
    fn irq_mutex_ownership_is_local_to_the_holding_thread() {
        let lock = IrqMutex::new(());
        assert!(!current_thread_holds_irq_mutex());

        let guard = lock.lock();
        assert!(current_thread_holds_irq_mutex());
        std::thread::scope(|scope| {
            scope.spawn(|| assert!(!current_thread_holds_irq_mutex()));
        });

        drop(guard);
        assert!(!current_thread_holds_irq_mutex());
    }
}
