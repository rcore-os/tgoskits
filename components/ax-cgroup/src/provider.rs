//! Kernel provider trait for cgroup operations.
//!
//! The kernel implements this trait to supply task/process primitives.
//! cgroup core logic calls these methods instead of reaching into
//! `crate::task::*` directly.

use alloc::sync::Arc;

use crate::CgroupNode;

/// Kernel provider that supplies task/process primitives to the cgroup subsystem.
///
/// The kernel must implement this trait and register a `&'static` instance
/// via [`crate::register_provider`] during boot.
pub trait CgroupProvider: Send + Sync {
    /// Returns `true` if the process with the given PID is in zombie state.
    fn is_zombie(&self, pid: u32) -> bool;

    /// Get the current cgroup assignment of a process.
    fn get_cgroup(&self, pid: u32) -> Option<Arc<CgroupNode>>;

    /// Set the cgroup assignment of a process.
    fn set_cgroup(&self, pid: u32, cgroup: Arc<CgroupNode>);
}

/// Internal cell for the provider singleton.
pub struct ProviderCell {
    inner: core::sync::atomic::AtomicPtr<ProviderSlot>,
}

struct ProviderSlot {
    provider: &'static dyn CgroupProvider,
}

// SAFETY: ProviderSlot is only accessed through the atomic pointer,
// and the provider is set once during init.
unsafe impl Send for ProviderSlot {}
unsafe impl Sync for ProviderSlot {}

impl Default for ProviderCell {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderCell {
    pub fn new() -> Self {
        Self {
            inner: core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    pub fn set(&self, provider: &'static dyn CgroupProvider) {
        let slot = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(ProviderSlot { provider }));
        self.inner
            .store(slot, core::sync::atomic::Ordering::Release);
    }

    pub fn get(&self) -> Option<&'static dyn CgroupProvider> {
        let ptr = self.inner.load(core::sync::atomic::Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            // SAFETY: The pointer was allocated with Box::into_raw and never freed.
            Some(unsafe { (*ptr).provider })
        }
    }
}
