//! Fixed-owner notification for one network queue executor.

use ax_task::{IrqWaitCell, IrqWorkerWaiter};

/// Sticky hard-IRQ event consumed by one CPU-pinned queue executor.
pub(super) struct QueueNotification {
    event: IrqWaitCell,
}

impl QueueNotification {
    pub(super) const fn new() -> Self {
        Self {
            event: IrqWaitCell::new(),
        }
    }

    pub(super) fn notify(&self) {
        let _result = self.event.notify();
    }

    pub(super) fn wait(&self, waiter: &IrqWorkerWaiter) {
        waiter
            .wait(&self.event)
            .unwrap_or_else(|error| panic!("network queue executor notification failed: {error}"));
    }
}
