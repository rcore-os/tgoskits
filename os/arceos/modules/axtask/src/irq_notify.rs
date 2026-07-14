use core::sync::atomic::{AtomicBool, Ordering};

use crate::WaitQueue;

/// IRQ-safe deferred notification primitive.
///
/// `IrqNotify` separates a hard-IRQ notification from the slow work that must
/// run in task context. IRQ handlers call [`notify_irq`](Self::notify_irq) to
/// publish a pending bit and wake a deferred worker. The worker then drains the
/// bit and performs the expensive work, such as polling a device or waking
/// `axpoll` waiters.
pub struct IrqNotify {
    pending: AtomicBool,
    wait: WaitQueue,
}

impl Default for IrqNotify {
    fn default() -> Self {
        Self::new()
    }
}

impl IrqNotify {
    /// Creates an empty notification object.
    pub const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            wait: WaitQueue::new(),
        }
    }

    /// Publishes a pending notification from IRQ context.
    ///
    /// This method is IRQ-safe: it does not allocate, does not call arbitrary
    /// wakers, and does not perform slow poll wakeups. Repeated notifications
    /// coalesce into one pending bit until a worker drains it.
    pub fn notify_irq(&self) {
        self.pending.store(true, Ordering::Release);
        self.wait.notify_one_from_irq();
    }

    /// Publishes a pending notification from task context.
    ///
    /// Prefer [`notify_irq`](Self::notify_irq) inside hard IRQ callbacks. This
    /// method exists for task/deferred code that wants the same coalescing
    /// behavior while still allowing the scheduler to observe a normal wake.
    pub fn notify(&self) {
        self.pending.store(true, Ordering::Release);
        self.wait.notify_one(true);
    }

    /// Returns whether a notification is currently pending.
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    /// Drains the pending bit.
    ///
    /// Returns `true` if at least one notification was pending.
    pub fn drain(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    /// Blocks until at least one pending notification is available, then drains it.
    #[track_caller]
    pub fn wait(&self) {
        self.wait.wait_until(|| self.drain());
    }

    /// Like [`wait`](Self::wait) but returns early after `dur` elapses.
    #[track_caller]
    pub fn wait_timeout(&self, dur: core::time::Duration) -> bool {
        self.wait.wait_timeout_until(dur, || self.drain())
    }
}
