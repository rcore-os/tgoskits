//! A non-poisoning sleeping mutex.

/// An alias of [`ax_runtime::sync::Mutex`].
pub type Mutex<T> = ax_runtime::sync::Mutex<T>;
/// An alias of [`ax_runtime::sync::MutexGuard`].
pub type MutexGuard<'a, T> = ax_runtime::sync::MutexGuard<'a, T>;
