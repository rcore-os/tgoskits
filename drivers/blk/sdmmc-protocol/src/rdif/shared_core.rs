use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) struct SharedCore<T> {
    pub(super) inner: Arc<SharedCoreInner<T>>,
}

pub(super) struct SharedCoreInner<T> {
    value: UnsafeCell<T>,
    borrowed: AtomicBool,
}

pub(super) struct SharedCoreGuard<'a, T> {
    inner: &'a SharedCoreInner<T>,
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

    pub(super) fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.inner.enter();
        f(guard.get_mut())
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
    pub(super) fn enter(&self) -> SharedCoreGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_enter() {
                return guard;
            }
            core::hint::spin_loop();
        }
    }

    fn try_enter(&self) -> Option<SharedCoreGuard<'_, T>> {
        self.borrowed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some(SharedCoreGuard { inner: self })
    }
}

impl<T> SharedCoreGuard<'_, T> {
    pub(super) fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.inner.value.get() }
    }
}

impl<T> Drop for SharedCoreGuard<'_, T> {
    fn drop(&mut self) {
        self.inner.borrowed.store(false, Ordering::Release);
    }
}
