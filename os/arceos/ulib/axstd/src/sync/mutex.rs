//! A non-poisoning sleeping mutex.

/// An alias of [`ax_sync::Mutex`].
pub type Mutex<T> = ax_sync::Mutex<T>;
/// An alias of [`ax_sync::MutexGuard`].
pub type MutexGuard<'a, T> = ax_sync::MutexGuard<'a, T>;
