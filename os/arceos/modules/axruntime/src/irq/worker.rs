use core::time::Duration;

use ax_lazyinit::OnceLock;
use ax_task::{
    sync::irq::{IrqWaitCell, IrqWorkerWaiter},
    thread::{TaskError, ThreadId, current::current_thread_handle},
};

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

    /// Publishes work from task or hard IRQ context without entering
    /// task-owned wait queues.
    pub fn notify(&self) {
        let _result = self.doorbell.notify();
    }

    /// Consumes one coalesced notification on the signal's fixed worker.
    pub fn wait(&self) -> Result<(), TaskError> {
        self.current_waiter()?.irq.wait(&self.doorbell)
    }

    /// Rechecks worker-owned state after a local timer expiry even when a
    /// producer cannot use the cross-CPU interrupt transport.
    pub(crate) fn wait_timeout(&self, timeout: Duration) -> Result<bool, TaskError> {
        self.current_waiter()?
            .irq
            .wait_timeout(&self.doorbell, timeout)
    }

    fn current_waiter(&self) -> Result<&FixedIrqWorkerWaiter, TaskError> {
        let current = current_thread_handle()?;
        let waiter = self.waiter.call_once(|| FixedIrqWorkerWaiter {
            owner: current.id(),
            #[cfg(feature = "fault-injection")]
            thread: current.clone(),
            irq: IrqWorkerWaiter::new(current.wake_handle()),
        });
        if waiter.owner != current.id() {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(waiter)
    }

    #[cfg(feature = "fault-injection")]
    pub(crate) fn is_blocked_for_test(&self) -> bool {
        self.waiter.get().is_some_and(|waiter| {
            waiter.thread.state() == ax_task::thread::ThreadState::Blocked
                && self.doorbell.has_waiter_for_test()
        })
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
    #[cfg(feature = "fault-injection")]
    thread: ax_task::thread::ThreadHandle,
    irq: IrqWorkerWaiter,
}
