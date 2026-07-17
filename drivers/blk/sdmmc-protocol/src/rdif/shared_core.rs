use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) struct SharedCore<T> {
    inner: Arc<SharedCoreInner<T>>,
}

pub(super) struct SharedCoreInner<T> {
    value: UnsafeCell<T>,
    borrowed: AtomicBool,
}

pub(super) struct SharedCoreGuard<'a, T> {
    inner: &'a SharedCoreInner<T>,
    // Model the exclusive borrow for auto-trait derivation. In particular, a
    // guard for `T: Send + !Sync` must not become `Sync` merely because the
    // lock-like inner object has a manual `Sync` implementation.
    value: PhantomData<&'a mut T>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SharedCoreBorrowError {
    Busy,
}

// SAFETY: `SharedCore` serializes queue/control access through a single atomic
// borrow flag. Hard IRQ callbacks own a separate host IRQ endpoint and never
// enter this shared card core.
unsafe impl<T: Send> Send for SharedCoreInner<T> {}

// SAFETY: See the `Send` impl.
unsafe impl<T: Send> Sync for SharedCoreInner<T> {}

impl<T> SharedCore<T> {
    pub(super) fn new(value: T) -> Self {
        Self {
            inner: Arc::new(SharedCoreInner {
                value: UnsafeCell::new(value),
                borrowed: AtomicBool::new(false),
            }),
        }
    }

    /// Try to acquire the task-side mutable endpoint exactly once.
    ///
    /// The caller decides whether contention means submit retry, another
    /// bounded service pass, or a lifecycle ordering violation. This gate
    /// never spins or sleeps because it can be reached by fixed workers whose
    /// progress must remain bounded.
    pub(super) fn try_borrow_mut(&self) -> Result<SharedCoreGuard<'_, T>, SharedCoreBorrowError> {
        self.inner.try_enter()
    }

    #[cfg(test)]
    pub(super) fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self
            .try_borrow_mut()
            .expect("serialized test access must not contend");
        f(&mut guard)
    }
}

impl<T> Clone for SharedCore<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> SharedCoreInner<T> {
    fn try_enter(&self) -> Result<SharedCoreGuard<'_, T>, SharedCoreBorrowError> {
        self.borrowed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| SharedCoreBorrowError::Busy)?;
        Ok(SharedCoreGuard {
            inner: self,
            value: PhantomData,
        })
    }
}

impl<T> Deref for SharedCoreGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: successful acquisition of `borrowed` gives this guard the
        // only live access to `value` until its Release store in `Drop`.
        unsafe { &*self.inner.value.get() }
    }
}

impl<T> DerefMut for SharedCoreGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: this guard uniquely owns the atomic mutable-borrow permit.
        unsafe { &mut *self.inner.value.get() }
    }
}

impl<T> Drop for SharedCoreGuard<'_, T> {
    fn drop(&mut self) {
        self.inner.borrowed.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contended_mutable_borrow_returns_busy_without_waiting() {
        let core = SharedCore::new(7_u32);
        let held = core
            .try_borrow_mut()
            .expect("the first mutable borrow must succeed");

        assert!(matches!(
            core.try_borrow_mut(),
            Err(SharedCoreBorrowError::Busy)
        ));

        drop(held);
        let mut value = core
            .try_borrow_mut()
            .expect("dropping the guard must publish availability");
        *value = 11;
        assert_eq!(*value, 11);
    }
}
