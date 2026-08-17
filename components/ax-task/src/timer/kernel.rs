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

/// Callback for a stable timer registration that may restart itself.
pub type RestartableKernelTimerCallback =
    Box<dyn FnMut(MonotonicInstant) -> KernelTimerAction + Send + 'static>;

/// Result returned by a restartable kernel-timer callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelTimerAction {
    /// Finish this registration after the current callback.
    Complete,
    /// Reinsert the same registration at a new absolute deadline.
    Rearm(MonotonicDeadline),
}

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
    callback: KernelTimerCallbackState,
}

enum KernelTimerCallbackState {
    OneShot(Option<KernelTimerCallback>),
    Restartable(RestartableKernelTimerCallback),
}

impl KernelTimerEntry {
    pub(crate) fn new(
        deadline: MonotonicDeadline,
        callback: KernelTimerCallback,
    ) -> Result<Self, TaskDeadlineError> {
        Ok(Self {
            identity: next_kernel_timer_identity()?,
            deadline,
            expired_at: None,
            callback: KernelTimerCallbackState::OneShot(Some(callback)),
        })
    }

    pub(crate) fn new_restartable(
        deadline: MonotonicDeadline,
        callback: RestartableKernelTimerCallback,
    ) -> Result<Self, TaskDeadlineError> {
        Ok(Self {
            identity: next_kernel_timer_identity()?,
            deadline,
            expired_at: None,
            callback: KernelTimerCallbackState::Restartable(callback),
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

    fn rearm(&mut self, deadline: MonotonicDeadline) {
        self.deadline = deadline;
        self.expired_at = None;
    }
}

fn next_kernel_timer_identity() -> Result<NonZeroU64, TaskDeadlineError> {
    let identity = NEXT_KERNEL_TIMER_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| TaskDeadlineError::GenerationExhausted)?;
    NonZeroU64::new(identity).ok_or(TaskDeadlineError::GenerationExhausted)
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

/// One callback claimed by the ktimer worker.
///
/// Cancellation while the callback runs leaves a tombstone that prevents a
/// restartable callback from returning the entry to the active queue.
pub(crate) struct KernelTimerExecution {
    entry: KernelTimerEntry,
}

impl KernelTimerExecution {
    #[cfg(feature = "task-test-hooks")]
    pub(crate) const fn handle(&self, owner: CpuId) -> KernelTimerHandle {
        KernelTimerHandle::new(owner, self.entry.identity())
    }

    pub(crate) fn invoke(&mut self) -> KernelTimerAction {
        let expired_at = self
            .entry
            .expired_at
            .expect("claimed kernel timer must have an expiry sample");
        match &mut self.entry.callback {
            KernelTimerCallbackState::OneShot(callback) => {
                callback
                    .take()
                    .expect("kernel timer callback may execute only once")(
                    expired_at
                );
                KernelTimerAction::Complete
            }
            KernelTimerCallbackState::Restartable(callback) => callback(expired_at),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExecutingKernelTimer {
    identity: NonZeroU64,
    cancel_requested: bool,
}

/// Result of one bounded hard-IRQ promotion pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KernelTimerExpireBatch {
    expired: usize,
    pending: bool,
}

impl KernelTimerExpireBatch {
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
    executing: Vec<ExecutingKernelTimer>,
    capacity: usize,
}

impl KernelTimerQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            active: Vec::with_capacity(capacity),
            expired: Vec::with_capacity(capacity),
            executing: Vec::with_capacity(capacity),
            capacity,
        }
    }

    pub(crate) fn insert(
        &mut self,
        owner: CpuId,
        entry: KernelTimerEntry,
    ) -> Result<KernelTimerHandle, KernelTimerEntry> {
        if self.active.len() + self.expired.len() + self.executing.len() >= self.capacity {
            return Err(entry);
        }
        let handle = KernelTimerHandle::new(owner, entry.identity());
        self.insert_at(entry);
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
        let removed = self
            .expired
            .iter()
            .position(|entry| entry.identity() == handle.identity())
            .map(|index| self.expired.remove(index));
        if removed.is_some() {
            return removed;
        }
        if let Some(executing) = self
            .executing
            .iter_mut()
            .find(|entry| entry.identity == handle.identity())
        {
            executing.cancel_requested = true;
        }
        None
    }

    pub(crate) fn restore_cancelled(&mut self, entry: KernelTimerEntry) {
        assert!(
            self.active.len() + self.expired.len() + self.executing.len() < self.capacity,
            "restoring a cancelled kernel timer must reuse its reserved capacity"
        );
        self.insert_at(entry);
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
        let entry = self.expired.remove(0);
        self.executing.push(ExecutingKernelTimer {
            identity: entry.identity(),
            cancel_requested: false,
        });
        Some(KernelTimerExecution { entry })
    }

    pub(crate) fn complete_execution(
        &mut self,
        mut execution: KernelTimerExecution,
        action: KernelTimerAction,
    ) -> Option<KernelTimerEntry> {
        let position = self
            .executing
            .iter()
            .position(|entry| entry.identity == execution.entry.identity())
            .expect("completed kernel timer must remain in executing state");
        let executing = self.executing.swap_remove(position);
        if !executing.cancel_requested
            && let KernelTimerAction::Rearm(deadline) = action
        {
            execution.entry.rearm(deadline);
            self.insert_at(execution.entry);
            return None;
        }
        Some(execution.entry)
    }

    fn insert_at(&mut self, entry: KernelTimerEntry) {
        let position = self.active.partition_point(|candidate| {
            (candidate.deadline(), candidate.identity()) > (entry.deadline(), entry.identity())
        });
        self.active.insert(position, entry);
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
        !self.active.is_empty() || !self.expired.is_empty() || !self.executing.is_empty()
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

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use alloc::{boxed::Box, sync::Arc};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn deadline(nanos: u64) -> MonotonicDeadline {
        MonotonicDeadline::from_nanos(nanos).unwrap()
    }

    fn instant(nanos: u64) -> MonotonicInstant {
        MonotonicInstant::from_nanos(nanos).unwrap()
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn restartable_timer_reuses_identity_until_cancelled() {
        let invocations = Arc::new(AtomicUsize::new(0));
        let callback_invocations = Arc::clone(&invocations);
        let entry = KernelTimerEntry::new_restartable(
            deadline(10),
            Box::new(move |_| {
                let invocation = callback_invocations.fetch_add(1, Ordering::Relaxed) + 1;
                KernelTimerAction::Rearm(deadline(10 + invocation as u64 * 10))
            }),
        )
        .unwrap();
        let mut queue = KernelTimerQueue::new(1);
        let handle = queue.insert(CpuId::new(0), entry).unwrap();

        assert_eq!(queue.expire_due(instant(10), 1).expired(), 1);
        let mut execution = queue.claim_expired().unwrap();
        let action = execution.invoke();
        assert!(queue.complete_execution(execution, action).is_none());
        assert_eq!(queue.next_deadline(), Some(deadline(20)));

        assert_eq!(queue.expire_due(instant(20), 1).expired(), 1);
        let mut execution = queue.claim_expired().unwrap();
        let action = execution.invoke();
        assert!(queue.complete_execution(execution, action).is_none());
        assert_eq!(queue.next_deadline(), Some(deadline(30)));
        assert_eq!(invocations.load(Ordering::Relaxed), 2);

        assert!(queue.cancel(handle).is_some());
        assert!(!queue.has_active_work());
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn cancellation_during_callback_prevents_restart() {
        let entry = KernelTimerEntry::new_restartable(
            deadline(10),
            Box::new(|_| KernelTimerAction::Rearm(deadline(20))),
        )
        .unwrap();
        let mut queue = KernelTimerQueue::new(1);
        let handle = queue.insert(CpuId::new(0), entry).unwrap();
        assert_eq!(queue.expire_due(instant(10), 1).expired(), 1);
        let mut execution = queue.claim_expired().unwrap();

        assert!(queue.cancel(handle).is_none());
        let action = execution.invoke();
        assert!(queue.complete_execution(execution, action).is_some());
        assert!(!queue.has_active_work());
    }
}
