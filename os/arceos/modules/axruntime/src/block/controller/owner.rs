//! Shutdown-lifetime controller ownership published to hctx and IRQ callbacks.

use core::{
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use rdif_block::RecoveryCause;

use super::BlockController;

/// Stable, allocation-free ownership edge from every hctx to its controller.
///
/// The link is allocated before queue construction and published before any
/// IRQ action can run. A controller is retained in the runtime registry for
/// the shutdown lifetime, so an Acquire load either observes no owner during
/// failed activation or a permanently stable [`BlockController`] address.
pub(in crate::block) struct ControllerOwnerLink {
    pub(super) owner: AtomicPtr<BlockController>,
}

impl ControllerOwnerLink {
    pub(super) const fn new() -> Self {
        Self {
            owner: AtomicPtr::new(ptr::null_mut()),
        }
    }

    pub(super) fn publish(&self, controller: &alloc::sync::Arc<BlockController>) {
        let owner = ptr::from_ref(controller.as_ref()).cast_mut();
        assert!(
            self.owner
                .compare_exchange(ptr::null_mut(), owner, Ordering::Release, Ordering::Relaxed)
                .is_ok(),
            "block controller owner link was published twice"
        );
    }

    pub(super) fn clear_after_drain(&self, controller: &BlockController) {
        let expected = ptr::from_ref(controller).cast_mut();
        let previous = self.owner.swap(ptr::null_mut(), Ordering::AcqRel);
        assert_eq!(previous, expected, "block controller owner link changed");
    }

    pub(in crate::block) fn request_recovery(&'static self, cause: RecoveryCause) -> bool {
        let owner = self.owner.load(Ordering::Acquire);
        if owner.is_null() {
            error!("block hctx faulted without a published controller owner");
            return false;
        }
        let controller: &'static BlockController = unsafe {
            // SAFETY: Release publication happens only after Arc construction.
            // Successful controllers are retained until shutdown; failed
            // activation clears this pointer only after IRQ and work drain.
            &*owner
        };
        controller.schedule_recovery(cause);
        true
    }

    /// Publishes an emergency queue fault from hard IRQ without entering the
    /// task-context recovery state machine on the interrupted stack.
    pub(in crate::block) fn request_irq_recovery(&'static self, queue_id: usize) -> bool {
        let owner = self.owner.load(Ordering::Acquire);
        if owner.is_null() {
            return false;
        }
        let controller: &'static BlockController = unsafe {
            // SAFETY: identical shutdown-lifetime publication contract to
            // request_recovery. This path only updates fixed atomics and
            // queues one preallocated recovery work item from hard IRQ.
            &*owner
        };
        controller.publish_irq_recovery(queue_id)
    }

    pub(in crate::block) fn wake_recovery(&'static self) {
        let owner = self.owner.load(Ordering::Acquire);
        if owner.is_null() {
            return;
        }
        let controller: &'static BlockController = unsafe {
            // SAFETY: identical to `request_recovery`; a drain wake remains
            // armed only while the published controller and IRQ registrations
            // are retained by the runtime registry.
            &*owner
        };
        let _ = controller.queue_recovery_work();
    }

    pub(super) fn record_lifecycle_irq(&'static self, source_id: usize) -> bool {
        let owner = self.owner.load(Ordering::Acquire);
        if owner.is_null() {
            return false;
        }
        let controller = unsafe {
            // SAFETY: identical shutdown-lifetime publication contract to
            // `wake_recovery`; hard IRQ never owns or releases the controller.
            &*owner
        };
        controller.record_lifecycle_irq(source_id)
    }

}

pub(super) unsafe fn controller_irq_drain_wake(data: usize) {
    let link = unsafe {
        // SAFETY: callback data names the shutdown-lifetime owner link. The
        // IRQ framework accepts only a static wake target for this callback.
        &*ptr::with_exposed_provenance::<ControllerOwnerLink>(data)
    };
    link.wake_recovery();
}
