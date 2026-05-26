use alloc::sync::Arc;
use core::cell::UnsafeCell;

pub(crate) struct SharedDriver<T> {
    inner: Arc<SharedDriverInner<T>>,
}

struct SharedDriverInner<T> {
    value: UnsafeCell<T>,
}

// SAFETY: `SharedDriver` is used for cross-kernel drivers whose task-side queue
// state and IRQ-control state are split by the driver contract. The wrapper
// centralizes the `UnsafeCell` boundary and only exposes scoped mutable access.
unsafe impl<T: Send> Send for SharedDriverInner<T> {}

// SAFETY: See the `Send` impl. Callers must keep shared-driver access within
// the functional split established by the block driver glue.
unsafe impl<T: Send> Sync for SharedDriverInner<T> {}

impl<T> SharedDriver<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            inner: Arc::new(SharedDriverInner {
                value: UnsafeCell::new(value),
            }),
        }
    }

    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        // SAFETY: The mutable reference is scoped to this closure and is not
        // returned to the caller. The IRQ path uses this without taking locks.
        let value = unsafe { &mut *self.inner.value.get() };
        f(value)
    }
}

impl<T> Clone for SharedDriver<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}
