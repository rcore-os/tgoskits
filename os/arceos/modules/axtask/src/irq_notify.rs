use alloc::{sync::Arc, vec::Vec};
use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
};

use ax_kernel_guard::NoPreemptIrqSave;

struct HardIrqSignalWaiter {
    task_id: u64,
    generation: u64,
    irq_waker: crate::HardIrqWaker,
    task_waker: crate::TaskWaker,
}

/// IRQ-safe deferred notification primitive.
///
/// `HardIrqSignal` separates a hard-IRQ notification from the slow work that must
/// run in task context. IRQ handlers call [`notify_irq`](Self::notify_irq) to
/// publish a pending bit and wake a deferred worker. The worker then drains the
/// bit and performs the expensive work, such as polling a device or waking
/// `axpoll` waiters.
pub struct HardIrqSignal {
    pending: AtomicBool,
    active_waiter: AtomicPtr<HardIrqSignalWaiter>,
    waiters: ax_kspin::SpinNoIrq<Vec<Arc<HardIrqSignalWaiter>>>,
}

impl Default for HardIrqSignal {
    fn default() -> Self {
        Self::new()
    }
}

impl HardIrqSignal {
    /// Creates an empty notification object.
    pub const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            active_waiter: AtomicPtr::new(ptr::null_mut()),
            waiters: ax_kspin::SpinNoIrq::new(Vec::new()),
        }
    }

    /// Publishes a pending notification from IRQ context.
    ///
    /// This method is IRQ-safe: it does not allocate, does not call arbitrary
    /// wakers, and does not perform slow poll wakeups. Repeated notifications
    /// coalesce into one pending bit until a worker drains it.
    pub fn notify_irq(&self) {
        self.pending.store(true, Ordering::Release);
        self.wake_active_from_irq();
    }

    fn wake_active_from_irq(&self) {
        let waiter = self.active_waiter.load(Ordering::Acquire);
        if !waiter.is_null() {
            // SAFETY: waiter nodes are ref-counted and retained in `self.waiters`
            // until the `HardIrqSignal` is dropped. IRQ producers must still follow
            // the owning device's normal disable/synchronize-before-drop rule.
            let _ = unsafe { &*waiter }.irq_waker.wake_from_irq(0);
        }
    }

    /// Publishes a pending notification from task context.
    ///
    /// Prefer [`notify_irq`](Self::notify_irq) inside hard IRQ callbacks. This
    /// method exists for task/deferred code that wants the same coalescing
    /// behavior while still allowing the scheduler to observe a normal wake.
    pub fn notify(&self) {
        self.pending.store(true, Ordering::Release);
        self.wake_active();
    }

    fn wake_active(&self) {
        let waiter = self.active_waiter.load(Ordering::Acquire);
        if !waiter.is_null() {
            // SAFETY: see `wake_active_from_irq`.
            let _ = unsafe { &*waiter }.task_waker.wake(0);
        }
    }

    /// Returns whether a notification is currently pending.
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    /// Arms the current task as the IRQ wake target without blocking.
    ///
    /// Use this when the actual sleep happens through another primitive, such as
    /// a timeout wait queue whose predicate checks [`is_pending`](Self::is_pending).
    #[track_caller]
    pub fn arm_current_task(&self) {
        self.init_hard_irq_waker();
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
        self.wait_until(|| false);
    }

    /// Blocks until `condition` becomes true or at least one pending notification
    /// is available, then drains the notification bit.
    #[track_caller]
    pub fn wait_until(&self, condition: impl Fn() -> bool) {
        crate::api::might_sleep();
        self.init_hard_irq_waker();
        loop {
            if self.should_stop_waiting(&condition) {
                return;
            }
            crate::current_run_queue::<NoPreemptIrqSave>()
                .future_blocked_resched(|| self.should_stop_waiting(&condition));
        }
    }

    fn should_stop_waiting(&self, condition: &impl Fn() -> bool) -> bool {
        let ready = condition();
        let notified = self.drain();
        ready || notified
    }

    fn init_hard_irq_waker(&self) {
        self.arm_task_waker(crate::current_task_waker());
    }

    fn arm_task_waker(&self, waker: crate::TaskWaker) {
        let task_id = waker.task_id();
        let generation = waker.generation();
        let mut waiters = self.waiters.lock();
        let waiter = if let Some(index) = waiters
            .iter()
            .position(|waiter| waiter.task_id == task_id && waiter.generation == generation)
        {
            Arc::as_ptr(&waiters[index]) as *mut HardIrqSignalWaiter
        } else {
            waiters.push(Arc::new(HardIrqSignalWaiter {
                task_id,
                generation,
                irq_waker: waker.to_hard_irq_waker(),
                task_waker: waker,
            }));
            Arc::as_ptr(waiters.last().expect("just pushed waiter")) as *mut HardIrqSignalWaiter
        };
        self.active_waiter.store(waiter, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn arm_irq_waker_for_test(&self, waker: crate::HardIrqWaker) {
        self.arm_task_waker(crate::TaskWaker::from_hard_irq_waker_for_test(waker));
    }
}
