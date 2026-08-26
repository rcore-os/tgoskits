use ax_lazyinit::OnceLock;

use super::{IrqWaitCell, IrqWorkerWaiter, TaskError, ThreadId, current_thread_handle};

/// One coalescing IRQ doorbell consumed by exactly one runtime worker.
///
/// Hard IRQ owns only publication to [`IrqWaitCell`]. Scheduler state and
/// task-context fanout remain owned by the fixed worker which calls
/// [`Self::wait`].
pub struct FixedIrqWorkerSignal {
    doorbell: IrqWaitCell,
    waiter: OnceLock<FixedIrqWorkerWaiter>,
}

impl FixedIrqWorkerSignal {
    pub const fn new() -> Self {
        Self {
            doorbell: IrqWaitCell::new(),
            waiter: OnceLock::new(),
        }
    }

    /// Publishes work from hard IRQ without entering task-owned wait queues.
    pub fn notify_from_irq(&self) {
        let _result = self.doorbell.notify();
    }

    /// Publishes work from task or deferred context.
    pub fn notify_from_task(&self) {
        let _result = self.doorbell.notify_from_task();
    }

    /// Consumes one coalesced notification on the signal's fixed worker.
    pub fn wait(&self) -> Result<(), TaskError> {
        let current = current_thread_handle()?;
        let waiter = self.waiter.call_once(|| FixedIrqWorkerWaiter {
            owner: current.id(),
            irq: IrqWorkerWaiter::new(current.wake_handle()),
        });
        if waiter.owner != current.id() {
            return Err(TaskError::InvalidConfiguration);
        }
        waiter.irq.wait(&self.doorbell)
    }

    #[cfg(test)]
    pub(crate) fn is_pending(&self) -> bool {
        self.doorbell.is_pending()
    }
}

impl Default for FixedIrqWorkerSignal {
    fn default() -> Self {
        Self::new()
    }
}

struct FixedIrqWorkerWaiter {
    owner: ThreadId,
    irq: IrqWorkerWaiter,
}
