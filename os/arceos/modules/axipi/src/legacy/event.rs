use alloc::{boxed::Box, sync::Arc};

/// One allocation-backed compatibility callback.
pub struct Callback(Box<dyn FnOnce() + Send>);

impl Callback {
    /// Creates a compatibility callback.
    pub fn new<F: FnOnce() + Send + 'static>(callback: F) -> Self {
        Self(Box::new(callback))
    }

    pub(crate) fn call(self) {
        (self.0)()
    }
}

impl<T: FnOnce() + Send + 'static> From<T> for Callback {
    fn from(callback: T) -> Self {
        Self::new(callback)
    }
}

/// An allocation-backed callback shared across multiple target CPUs.
#[derive(Clone)]
pub struct MulticastCallback(Arc<dyn Fn() + Send + Sync>);

impl MulticastCallback {
    /// Creates a multicast compatibility callback.
    pub fn new<F: Fn() + Send + Sync + 'static>(callback: F) -> Self {
        Self(Arc::new(callback))
    }

    pub(crate) fn into_unicast(self) -> Callback {
        Callback(Box::new(move || (self.0)()))
    }

    pub(crate) fn call(self) {
        (self.0)()
    }
}

impl<T: Fn() + Send + Sync + 'static> From<T> for MulticastCallback {
    fn from(callback: T) -> Self {
        Self::new(callback)
    }
}

pub(super) struct IpiEvent {
    pub(super) source_cpu: usize,
    pub(super) callback: Callback,
}
