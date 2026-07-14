#[cfg(not(test))]
pub use ax_sync::{PiMutex, PiMutexGuard, SpinMutex, SpinMutexGuard};
#[cfg(test)]
pub use tests::{
    TestMutex as PiMutex, TestMutex as SpinMutex, TestMutexGuard as PiMutexGuard,
    TestMutexGuard as SpinMutexGuard,
};

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
        pub const fn new(value: T) -> Self {
            Self(Mutex::new(value))
        }

        pub fn into_inner(self) -> T {
            self.0.into_inner().unwrap_or_else(|err| err.into_inner())
        }
    }

    impl<T: ?Sized> TestMutex<T> {
        pub fn lock(&self) -> TestMutexGuard<'_, T> {
            TestMutexGuard(self.0.lock().unwrap_or_else(|err| err.into_inner()))
        }

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
