//! A naïve sleeping mutex.

use crate::os::arceos::task::AxRawMutex;

/// An alias of [`lock_api::Mutex`].
pub type Mutex<T> = lock_api::Mutex<AxRawMutex, T>;
/// An alias of [`lock_api::MutexGuard`].
pub type MutexGuard<'a, T> = lock_api::MutexGuard<'a, AxRawMutex, T>;
