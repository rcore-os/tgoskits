//! Move-only publication of faultable user-memory access state.

use alloc::rc::Rc;
use core::{
    marker::PhantomData,
    sync::atomic::{AtomicU32, Ordering},
};

/// Nesting state for one thread's faultable user-memory access.
pub(crate) struct UserMemoryAccessDepth(AtomicU32);

impl UserMemoryAccessDepth {
    /// Creates an inactive access depth.
    pub(crate) const fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    /// Publishes one access scope and returns its unique removal capability.
    pub(crate) fn enter(&self) -> UserMemoryAccessGuard<'_> {
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |depth| {
                depth.checked_add(1)
            })
            .expect("user-memory access nesting overflow");
        UserMemoryAccessGuard {
            depth: self,
            _not_send: PhantomData,
        }
    }

    /// Returns whether at least one access scope is active.
    pub(crate) fn is_active(&self) -> bool {
        self.0.load(Ordering::Acquire) != 0
    }

    fn leave(&self) {
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |depth| {
                depth.checked_sub(1)
            })
            .expect("unbalanced user-memory access scope");
    }
}

/// The unique capability that removes one user-memory access publication.
#[must_use = "dropping the guard closes the user-memory access scope"]
pub(crate) struct UserMemoryAccessGuard<'depth> {
    depth: &'depth UserMemoryAccessDepth,
    _not_send: PhantomData<Rc<()>>,
}

impl Drop for UserMemoryAccessGuard<'_> {
    fn drop(&mut self) {
        self.depth.leave();
    }
}
