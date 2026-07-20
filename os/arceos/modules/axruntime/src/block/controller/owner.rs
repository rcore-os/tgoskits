//! Weak queue-to-controller recovery publication.

use alloc::sync::{Arc, Weak};

use ax_kspin::SpinNoPreempt;
use rdif_block::RecoveryCause;

use super::BlockController;

/// Non-owning edge used by queues to notify their maintenance owner.
///
/// The weak reference avoids both a controller/queue cycle and the previous
/// shutdown-lifetime raw-pointer allocation. A controller explicitly clears
/// the edge only after admission, IRQ callbacks, and owner service are drained.
pub(in crate::block) struct ControllerOwnerLink {
    owner: SpinNoPreempt<Weak<BlockController>>,
}

impl ControllerOwnerLink {
    pub(super) fn new() -> Self {
        Self {
            owner: SpinNoPreempt::new(Weak::new()),
        }
    }

    pub(super) fn publish(&self, controller: &Arc<BlockController>) {
        let mut owner = self.owner.lock();
        assert!(
            owner.strong_count() == 0,
            "block controller owner link was published twice"
        );
        *owner = Arc::downgrade(controller);
    }

    pub(super) fn clear_after_drain(&self, controller: &BlockController) {
        let mut owner = self.owner.lock();
        if let Some(published) = owner.upgrade() {
            assert!(
                core::ptr::eq(published.as_ref(), controller),
                "block controller owner link changed"
            );
        }
        *owner = Weak::new();
    }

    pub(in crate::block) fn request_recovery(&self, cause: RecoveryCause) -> bool {
        let Some(controller) = self.controller() else {
            return false;
        };
        controller.schedule_recovery(cause);
        true
    }

    pub(in crate::block) fn request_irq_recovery(&self, queue_id: usize) -> bool {
        self.controller()
            .is_some_and(|controller| controller.publish_irq_recovery(queue_id))
    }

    pub(in crate::block) fn wake_recovery(&self) {
        if let Some(controller) = self.controller() {
            let _ = controller
                .maintenance
                .publish_cause(super::BLOCK_OWNER_CONTROL_CAUSE);
        }
    }

    fn controller(&self) -> Option<Arc<BlockController>> {
        self.owner.lock().upgrade()
    }
}
