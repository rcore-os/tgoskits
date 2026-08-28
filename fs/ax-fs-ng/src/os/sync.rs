#[cfg(not(test))]
pub use ax_sync::{Mutex as SleepMutex, MutexGuard as SleepMutexGuard};
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
pub use tests::{TestMutex as SleepMutex, TestMutexGuard as SleepMutexGuard};

#[cfg(test)]
mod tests {
    use core::{
        fmt,
        ops::{Deref, DerefMut},
    };
    use std::sync::{Mutex, MutexGuard, TryLockError};

    pub struct TestMutex<T: ?Sized>(Mutex<T>);

    pub struct TestMutexGuard<'a, T: ?Sized>(MutexGuard<'a, T>);

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
