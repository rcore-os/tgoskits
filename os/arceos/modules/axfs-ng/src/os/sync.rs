#[cfg(not(test))]
pub use ax_kspin::{SpinNoIrq as IrqMutex, SpinNoIrqGuard as IrqMutexGuard};
#[cfg(not(test))]
pub use ax_sync::{Mutex as SleepMutex, MutexGuard as SleepMutexGuard};
#[cfg(test)]
pub use test_sync::{
    TestMutex as IrqMutex, TestMutex as SleepMutex, TestMutexGuard as IrqMutexGuard,
    TestMutexGuard as SleepMutexGuard,
};

#[cfg(test)]
mod test_sync {
    use std::sync::{Mutex as StdMutex, MutexGuard as StdMutexGuard, TryLockError};

    pub struct TestMutex<T> {
        inner: StdMutex<T>,
    }

    pub type TestMutexGuard<'a, T> = StdMutexGuard<'a, T>;

    impl<T> TestMutex<T> {
        pub const fn new(value: T) -> Self {
            Self {
                inner: StdMutex::new(value),
            }
        }

        pub fn lock(&self) -> TestMutexGuard<'_, T> {
            self.inner.lock().unwrap_or_else(|err| err.into_inner())
        }

        pub fn try_lock(&self) -> Option<TestMutexGuard<'_, T>> {
            match self.inner.try_lock() {
                Ok(guard) => Some(guard),
                Err(TryLockError::Poisoned(err)) => Some(err.into_inner()),
                Err(TryLockError::WouldBlock) => None,
            }
        }
    }
}
