use alloc::{boxed::Box, sync::Arc};

/// A callback function that will be called when an [`IpiEvent`] is received and handled.
///
/// IPI callbacks execute with local IRQs disabled and must not block, allocate,
/// fault, or acquire non-IRQ-safe locks.
pub struct Callback(Box<dyn FnOnce() + Send>);

impl Callback {
    /// Create a new [`Callback`] with the given function.
    pub fn new<F: FnOnce() + Send + 'static>(callback: F) -> Self {
        Self(Box::new(callback))
    }

    /// Call the callback function.
    pub fn call(self) {
        (self.0)()
    }
}

impl<T: FnOnce() + Send + 'static> From<T> for Callback {
    fn from(callback: T) -> Self {
        Self::new(callback)
    }
}

/// A [`Callback`] that can be called multiple times. It's used for multicast IPI events.
///
/// Every invocation follows the same IRQ-safe, non-blocking contract as [`Callback`].
#[derive(Clone)]
pub struct MulticastCallback(Arc<dyn Fn() + Send + Sync>);

impl MulticastCallback {
    /// Create a new [`MulticastCallback`] with the given function.
    pub fn new<F: Fn() + Send + Sync + 'static>(callback: F) -> Self {
        Self(Arc::new(callback))
    }

    /// Convert the [`MulticastCallback`] into a [`Callback`].
    pub fn into_unicast(self) -> Callback {
        Callback(Box::new(move || (self.0)()))
    }

    /// Call the callback function.
    pub fn call(self) {
        (self.0)()
    }
}

impl<T: Fn() + Send + Sync + 'static> From<T> for MulticastCallback {
    fn from(callback: T) -> Self {
        Self::new(callback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn callback_ownership_can_cross_the_ipi_queue_boundary() {
        assert_send::<Callback>();
        assert_send_sync::<MulticastCallback>();
    }
}
