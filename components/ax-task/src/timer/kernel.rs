//! Task-context kernel callbacks sharing the scheduler clockevent owner.

use alloc::{boxed::Box, vec::Vec};
use core::{
    fmt,
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use super::TaskDeadlineError;
use crate::{
    CpuId,
    runtime::{MonotonicDeadline, MonotonicInstant},
};

static NEXT_KERNEL_TIMER_ID: AtomicU64 = AtomicU64::new(1);

/// Callback executed by the owner CPU's `ktimers/%u` service thread.
pub type KernelTimerCallback = Box<dyn FnOnce(MonotonicInstant) + Send + 'static>;

/// Stable identity of one host kernel-timer registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KernelTimerHandle {
    owner: CpuId,
    identity: NonZeroU64,
}

impl KernelTimerHandle {
    pub(crate) const fn new(owner: CpuId, identity: NonZeroU64) -> Self {
        Self { owner, identity }
    }

    /// Returns the CPU deadline base that owns this registration.
    pub const fn owner(self) -> CpuId {
        self.owner
    }

    pub(crate) const fn identity(self) -> NonZeroU64 {
        self.identity
    }
}

/// Outcome of a non-blocking kernel-timer cancellation attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelTimerCancelOutcome {
    /// The queued or expired callback was removed before execution.
    Cancelled,
    /// The handle was already cancelled, claimed for execution, or completed.
    NotCancelled,
}

pub(crate) struct KernelTimerEntry {
    identity: NonZeroU64,
    deadline: MonotonicDeadline,
    expired_at: Option<MonotonicInstant>,
    callback: Option<KernelTimerCallback>,
}

impl KernelTimerEntry {
    pub(crate) fn new(
        deadline: MonotonicDeadline,
        callback: KernelTimerCallback,
    ) -> Result<Self, TaskDeadlineError> {
        let identity = NEXT_KERNEL_TIMER_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| TaskDeadlineError::GenerationExhausted)?;
        let identity = NonZeroU64::new(identity).ok_or(TaskDeadlineError::GenerationExhausted)?;
        Ok(Self {
            identity,
            deadline,
            expired_at: None,
            callback: Some(callback),
        })
    }

    const fn deadline(&self) -> MonotonicDeadline {
        self.deadline
    }

    const fn identity(&self) -> NonZeroU64 {
        self.identity
    }

    fn expire(&mut self, now: MonotonicInstant) {
        assert!(self.expired_at.replace(now).is_none());
    }
}

impl fmt::Debug for KernelTimerEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KernelTimerEntry")
            .field("identity", &self.identity)
            .field("deadline", &self.deadline)
            .field("expired_at", &self.expired_at)
            .finish_non_exhaustive()
    }
}

/// One callback claimed by the ktimer worker and no longer cancellable.
pub(crate) struct KernelTimerExecution {
    entry: KernelTimerEntry,
}

impl KernelTimerExecution {
    pub(crate) fn invoke(&mut self) {
        let expired_at = self
            .entry
            .expired_at
            .expect("claimed kernel timer must have an expiry sample");
        let callback = self
            .entry
            .callback
            .take()
            .expect("kernel timer callback may execute only once");
        callback(expired_at);
    }
}

/// Result of one bounded hard-IRQ promotion pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KernelTimerExpireBatch {
    expired: usize,
    pending: bool,
}

impl KernelTimerExpireBatch {
    pub(crate) const fn empty() -> Self {
        Self {
            expired: 0,
            pending: false,
        }
    }

    pub(crate) const fn expired(self) -> usize {
        self.expired
    }

    pub(crate) const fn pending(self) -> bool {
        self.pending
    }
}

/// Fixed-capacity kernel callback clock base.
///
/// Callback ownership is allocated before this queue is locked. Expiry only
/// moves entries between preallocated vectors, so hard IRQ never
/// allocates, frees, or invokes arbitrary code.
pub(crate) struct KernelTimerQueue {
    active: Vec<KernelTimerEntry>,
    expired: Vec<KernelTimerEntry>,
    executing: usize,
    capacity: usize,
}

impl KernelTimerQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            active: Vec::with_capacity(capacity),
            expired: Vec::with_capacity(capacity),
            executing: 0,
            capacity,
        }
    }

    pub(crate) fn insert(
        &mut self,
        owner: CpuId,
        entry: KernelTimerEntry,
    ) -> Result<KernelTimerHandle, KernelTimerEntry> {
        if self.active.len() + self.expired.len() + self.executing >= self.capacity {
            return Err(entry);
        }
        let handle = KernelTimerHandle::new(owner, entry.identity());
        let position = self.active.partition_point(|candidate| {
            (candidate.deadline(), candidate.identity()) > (entry.deadline(), entry.identity())
        });
        self.active.insert(position, entry);
        Ok(handle)
    }

    pub(crate) fn cancel(&mut self, handle: KernelTimerHandle) -> Option<KernelTimerEntry> {
        if let Some(index) = self
            .active
            .iter()
            .position(|entry| entry.identity() == handle.identity())
        {
            return Some(self.active.remove(index));
        }
        self.expired
            .iter()
            .position(|entry| entry.identity() == handle.identity())
            .map(|index| self.expired.remove(index))
    }

    pub(crate) fn expire_due(
        &mut self,
        now: MonotonicInstant,
        budget: usize,
    ) -> KernelTimerExpireBatch {
        let mut expired = 0;
        while expired < budget
            && self
                .active
                .last()
                .is_some_and(|entry| now.reached(entry.deadline()))
        {
            let mut entry = self
                .active
                .pop()
                .expect("due kernel timer must remain in the active queue");
            entry.expire(now);
            self.expired.push(entry);
            expired += 1;
        }
        KernelTimerExpireBatch {
            expired,
            pending: self.has_due(now),
        }
    }

    pub(crate) fn claim_expired(&mut self) -> Option<KernelTimerExecution> {
        if self.expired.is_empty() {
            return None;
        }
        self.executing += 1;
        Some(KernelTimerExecution {
            entry: self.expired.remove(0),
        })
    }

    pub(crate) fn complete_execution(&mut self) {
        self.executing = self
            .executing
            .checked_sub(1)
            .expect("kernel timer execution accounting underflowed");
    }

    pub(crate) fn next_deadline(&self) -> Option<MonotonicDeadline> {
        self.active.last().map(|entry| entry.deadline())
    }

    pub(crate) fn has_due(&self, now: MonotonicInstant) -> bool {
        self.active
            .last()
            .is_some_and(|entry| now.reached(entry.deadline()))
    }

    pub(crate) fn has_expired(&self) -> bool {
        !self.expired.is_empty()
    }

    pub(crate) fn has_active_work(&self) -> bool {
        !self.active.is_empty() || !self.expired.is_empty() || self.executing != 0
    }
}

impl fmt::Debug for KernelTimerQueue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KernelTimerQueue")
            .field("active", &self.active)
            .field("expired", &self.expired)
            .field("executing", &self.executing)
            .field("capacity", &self.capacity)
            .finish()
    }
}
