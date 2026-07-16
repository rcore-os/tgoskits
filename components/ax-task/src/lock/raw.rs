//! FIFO ticket lock independent from OS and third-party lock crates.

use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicUsize, Ordering},
};

/// A FIFO raw ticket lock used only inside `ax-task`.
#[derive(Debug)]
pub(crate) struct RawTicketLock<T> {
    next: AtomicUsize,
    owner: AtomicUsize,
    value: UnsafeCell<T>,
}

impl<T> RawTicketLock<T> {
    /// Creates an unlocked ticket lock.
    pub(crate) const fn new(value: T) -> Self {
        Self {
            next: AtomicUsize::new(0),
            owner: AtomicUsize::new(0),
            value: UnsafeCell::new(value),
        }
    }

    /// Acquires the lock in ticket order.
    pub(crate) fn lock(&self) -> RawTicketGuard<'_, T> {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        while self.owner.load(Ordering::Acquire) != ticket {
            spin_loop();
        }
        RawTicketGuard {
            lock: self,
            _not_send: PhantomData,
        }
    }

    /// Attempts immediate acquisition without waiting.
    pub(crate) fn try_lock(&self) -> Option<RawTicketGuard<'_, T>> {
        let owner = self.owner.load(Ordering::Acquire);
        self.next
            .compare_exchange(
                owner,
                owner.wrapping_add(1),
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .ok()
            .map(|_| RawTicketGuard {
                lock: self,
                _not_send: PhantomData,
            })
    }

    fn unlock(&self) {
        self.owner.fetch_add(1, Ordering::Release);
    }
}

// SAFETY: moving the lock transfers ownership of `T`; no access is possible
// without either ownership or a successfully acquired ticket.
unsafe impl<T: Send> Send for RawTicketLock<T> {}

// SAFETY: the ticket protocol provides exclusive mutable access. Sharing the
// lock is sound when the protected value may move between execution contexts.
unsafe impl<T: Send> Sync for RawTicketLock<T> {}

/// Exclusive access returned by [`RawTicketLock::lock`].
pub(crate) struct RawTicketGuard<'a, T> {
    lock: &'a RawTicketLock<T>,
    // Lock ownership is execution-context local even though the lock is Sync.
    _not_send: PhantomData<*mut ()>,
}

impl<T> Deref for RawTicketGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: this guard owns the currently served unique ticket.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for RawTicketGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: this guard owns the currently served unique ticket.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for RawTicketGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.unlock();
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use super::*;

    #[test]
    fn try_lock_does_not_consume_a_ticket_on_failure() {
        let lock = RawTicketLock::new(0usize);
        let first = lock.lock();
        assert!(lock.try_lock().is_none());
        drop(first);
        let mut second = lock.try_lock().expect("failed try-lock must roll back");
        *second = 1;
        assert_eq!(*second, 1);
    }

    #[test]
    fn serializes_concurrent_writers() {
        let lock = Arc::new(RawTicketLock::new(0usize));
        let workers: alloc::vec::Vec<_> = (0..4)
            .map(|_| {
                let lock = Arc::clone(&lock);
                std::thread::spawn(move || {
                    for _ in 0..1_000 {
                        *lock.lock() += 1;
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("writer thread panicked");
        }
        assert_eq!(*lock.lock(), 4_000);
    }
}
