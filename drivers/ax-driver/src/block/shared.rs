use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};

pub(crate) struct SharedDriver<T> {
    inner: Arc<SharedDriverInner<T>>,
}

struct SharedDriverInner<T> {
    value: UnsafeCell<T>,
    borrowed: AtomicBool,
}

struct SharedDriverGuard<'a, T> {
    inner: &'a SharedDriverInner<T>,
}

// SAFETY: `SharedDriver` centralizes the `UnsafeCell` boundary. Access to the
// inner value is serialized by `borrowed`; IRQ users must use `try_with_mut`,
// which never spins or blocks on the callback path.
unsafe impl<T: Send> Send for SharedDriverInner<T> {}

// SAFETY: See the `Send` impl.
unsafe impl<T: Send> Sync for SharedDriverInner<T> {}

impl<T> SharedDriver<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            inner: Arc::new(SharedDriverInner {
                value: UnsafeCell::new(value),
                borrowed: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.inner.enter();
        f(guard.get_mut())
    }

    #[cfg(any(
        feature = "k230-sdhci",
        feature = "phytium-mci",
        feature = "rockchip-dwmmc",
        feature = "rockchip-sdhci"
    ))]
    pub(crate) fn try_with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut guard = self.inner.try_enter()?;
        Some(f(guard.get_mut()))
    }
}

impl<T> Clone for SharedDriver<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> SharedDriverInner<T> {
    fn enter(&self) -> SharedDriverGuard<'_, T> {
        loop {
            if let Some(guard) = self.try_enter() {
                return guard;
            }
            core::hint::spin_loop();
        }
    }

    fn try_enter(&self) -> Option<SharedDriverGuard<'_, T>> {
        self.borrowed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some(SharedDriverGuard { inner: self })
    }
}

impl<T> SharedDriverGuard<'_, T> {
    fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.inner.value.get() }
    }
}

impl<T> Drop for SharedDriverGuard<'_, T> {
    fn drop(&mut self) {
        self.inner.borrowed.store(false, Ordering::Release);
    }
}
