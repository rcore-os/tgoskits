//! A non-poisoning sleeping mutex.

/// An alias of [`ax_runtime::task::sync::Mutex`].
pub type Mutex<T> = ax_runtime::task::sync::Mutex<T>;
/// An alias of [`ax_runtime::task::sync::MutexGuard`].
pub type MutexGuard<'a, T> = ax_runtime::task::sync::MutexGuard<'a, T>;
