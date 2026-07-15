//! Sticky notification and single-consumer ownership for deferred task work.

#[cfg(test)]
use core::sync::atomic::AtomicPtr;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::{IrqWaitCell, TaskError};

const WORKER_UNINSTALLED: u8 = 0;
const WORKER_STARTING: u8 = 1;
const WORKER_INSTALLED: u8 = 2;

/// Allocation-free doorbell shared by scheduler producers and the reaper.
#[derive(Debug)]
pub(crate) struct TaskWorkDoorbell {
    event: IrqWaitCell,
    pending: AtomicBool,
    consumer_active: AtomicBool,
    worker_state: AtomicU8,
    #[cfg(test)]
    publish_barrier: AtomicPtr<TestPublishBarrier>,
}

impl TaskWorkDoorbell {
    pub(crate) const fn new() -> Self {
        Self {
            event: IrqWaitCell::new(),
            pending: AtomicBool::new(false),
            consumer_active: AtomicBool::new(false),
            worker_state: AtomicU8::new(WORKER_UNINSTALLED),
            #[cfg(test)]
            publish_barrier: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// Publishes work before waking the fixed service thread.
    pub(crate) fn publish(&self) {
        self.pending.store(true, Ordering::Release);
        #[cfg(test)]
        self.wait_at_test_publish_barrier();
        let _notified = self.event.notify();
    }

    #[cfg(test)]
    pub(crate) fn install_test_publish_barrier(&self, barrier: &'static TestPublishBarrier) {
        self.publish_barrier
            .store(core::ptr::from_ref(barrier).cast_mut(), Ordering::Release);
    }

    #[cfg(test)]
    fn wait_at_test_publish_barrier(&self) {
        let barrier = self.publish_barrier.load(Ordering::Acquire);
        if !barrier.is_null() {
            // SAFETY: test installation requires a leaked, shutdown-lifetime
            // barrier, so the pointer remains valid for this TaskSystem.
            unsafe { &*barrier }.wait();
        }
    }

    pub(crate) fn take_pending(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn reassert_pending(&self) {
        self.pending.store(true, Ordering::Release);
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire) || self.event.is_pending()
    }

    pub(crate) const fn event(&self) -> &IrqWaitCell {
        &self.event
    }

    pub(crate) fn try_claim_consumer(&self) -> Result<TaskWorkConsumerGuard<'_>, TaskError> {
        self.consumer_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| TaskError::ThreadBusy)?;
        Ok(TaskWorkConsumerGuard { doorbell: self })
    }

    pub(crate) fn begin_worker_install(&self) -> Result<(), TaskError> {
        self.worker_state
            .compare_exchange(
                WORKER_UNINSTALLED,
                WORKER_STARTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| TaskError::InvalidConfiguration)
    }

    pub(crate) fn finish_worker_install(&self) {
        let previous = self.worker_state.swap(WORKER_INSTALLED, Ordering::AcqRel);
        assert_eq!(
            previous, WORKER_STARTING,
            "task-work worker completed installation from an invalid state"
        );
        self.publish();
    }

    pub(crate) fn cancel_worker_install(&self) {
        let previous = self.worker_state.swap(WORKER_UNINSTALLED, Ordering::AcqRel);
        assert_eq!(
            previous, WORKER_STARTING,
            "task-work worker cancelled installation from an invalid state"
        );
    }
}

#[cfg(test)]
pub(crate) struct TestPublishBarrier {
    entered: AtomicBool,
    released: AtomicBool,
}

#[cfg(test)]
impl TestPublishBarrier {
    pub(crate) const fn new() -> Self {
        Self {
            entered: AtomicBool::new(false),
            released: AtomicBool::new(false),
        }
    }

    fn wait(&self) {
        self.entered.store(true, Ordering::Release);
        while !self.released.load(Ordering::Acquire) {
            core::hint::spin_loop();
        }
    }

    pub(crate) fn wait_until_entered(&self) {
        while !self.entered.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
    }

    pub(crate) fn release(&self) {
        self.released.store(true, Ordering::Release);
    }
}

pub(crate) struct TaskWorkConsumerGuard<'doorbell> {
    doorbell: &'doorbell TaskWorkDoorbell,
}

impl Drop for TaskWorkConsumerGuard<'_> {
    fn drop(&mut self) {
        assert!(
            self.doorbell.consumer_active.swap(false, Ordering::Release),
            "task-work consumer released without ownership"
        );
    }
}
