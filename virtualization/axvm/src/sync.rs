//! Synchronization helpers for task-context standard-library locks.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Recovers a standard-library mutex guard after host-side unwinding.
///
/// Axvisor production builds use `panic_abort`, so mutex poisoning cannot be
/// observed there. Host tests may unwind, and retaining the protected value is
/// more useful than turning one failed test into unrelated follow-on failures.
pub(crate) trait MutexExt<T: ?Sized> {
    fn lock_unpoisoned(&self) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> MutexExt<T> for Mutex<T> {
    fn lock_unpoisoned(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex, OnceLock},
        thread,
    };

    use super::MutexExt;

    #[test]
    fn lock_recovers_the_value_after_host_unwinding() {
        let value = Arc::new(Mutex::new(7));
        let poisoned = value.clone();
        let _ = thread::spawn(move || {
            let _guard = poisoned.lock_unpoisoned();
            panic!("poison the host mutex");
        })
        .join();

        assert_eq!(*value.lock_unpoisoned(), 7);
    }

    #[test]
    fn once_lock_retries_after_failed_initialization() {
        let value = OnceLock::new();

        assert_eq!(
            value.get_or_try_init(|| Err::<usize, _>("retry")),
            Err("retry")
        );
        assert_eq!(value.get_or_try_init(|| Ok::<usize, &str>(11)), Ok(&11));
        assert_eq!(value.get(), Some(&11));
    }
}
