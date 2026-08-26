//! Sticky notification and single-consumer ownership for deferred task work.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use crate::{IrqWaitCell, TaskError};

const WORKER_UNINSTALLED: u8 = 0;
const WORKER_STARTING: u8 = 1;
const WORKER_INSTALLED: u8 = 2;

/// Allocation-free doorbell shared by scheduler producers and the reaper.
#[derive(Debug)]
pub(crate) struct TaskWorkDoorbell {
    event: IrqWaitCell,
    published_epoch: AtomicU64,
    claimed_epoch: AtomicU64,
    consumer_active: AtomicBool,
    worker_state: AtomicU8,
}

impl TaskWorkDoorbell {
    pub(crate) const fn new() -> Self {
        Self {
            event: IrqWaitCell::new(),
            published_epoch: AtomicU64::new(0),
            claimed_epoch: AtomicU64::new(0),
            consumer_active: AtomicBool::new(false),
            worker_state: AtomicU8::new(WORKER_UNINSTALLED),
        }
    }

    /// Publishes work before waking the fixed service thread.
    pub(crate) fn publish(&self) {
        let previous = self.advance_published_epoch();
        #[cfg(feature = "qperf-metrics")]
        {
            let edge = previous == self.claimed_epoch.load(Ordering::Acquire);
            crate::metrics::record_task_work_publish(edge);
        }
        #[cfg(not(feature = "qperf-metrics"))]
        let _ = previous;
        let _notified = self.event.notify();
    }

    pub(crate) fn claim_pending(&self) -> Option<TaskWorkClaim> {
        let claim = loop {
            let claimed = self.claimed_epoch.load(Ordering::Acquire);
            let published = self.published_epoch.load(Ordering::Acquire);
            if claimed == published {
                break None;
            }
            if self
                .claimed_epoch
                .compare_exchange(claimed, published, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break Some(TaskWorkClaim { epoch: published });
            }
        };
        #[cfg(feature = "qperf-metrics")]
        if claim.is_some() {
            crate::metrics::record_task_work_pending_consumed();
        }
        claim
    }

    pub(crate) fn reassert_pending(&self) {
        self.advance_published_epoch();
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_task_work_reassertion();
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.published_epoch.load(Ordering::Acquire) != self.claimed_epoch.load(Ordering::Acquire)
            || self.event.is_pending()
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

    fn advance_published_epoch(&self) -> u64 {
        self.published_epoch
            .try_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("task-work publication epoch exhausted"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TaskWorkClaim {
    epoch: u64,
}

impl TaskWorkClaim {
    pub(crate) const fn epoch(self) -> u64 {
        self.epoch
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
