//! Task-context kernel callbacks sharing the scheduler clockevent owner.

use alloc::{boxed::Box, vec::Vec};
use core::{
    fmt,
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static NEXT_KERNEL_TIMER_ID: AtomicU64 = AtomicU64::new(1);

/// Dense logical CPU identity owning one kernel timer registration.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TimerCpuId(usize);

impl TimerCpuId {
    pub const fn new(cpu_id: usize) -> Self {
        Self(cpu_id)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }
}

/// Finite absolute deadline in the host monotonic clock domain.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicDeadline(Duration);

impl MonotonicDeadline {
    pub fn from_duration(deadline: Duration) -> Result<Self, KernelTimerError> {
        let nanos = deadline.as_nanos();
        if nanos >= u128::from(u64::MAX) {
            return Err(KernelTimerError::InvalidDeadline);
        }
        Ok(Self(deadline))
    }

    pub fn from_nanos(nanos: u64) -> Result<Self, KernelTimerError> {
        Self::from_duration(Duration::from_nanos(nanos))
    }

    pub const fn as_duration(self) -> Duration {
        self.0
    }
}

/// Sample from the same monotonic clock domain as [`MonotonicDeadline`].
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonotonicInstant(Duration);

impl MonotonicInstant {
    pub fn from_duration(now: Duration) -> Result<Self, KernelTimerError> {
        let nanos = now.as_nanos();
        if nanos >= u128::from(u64::MAX) {
            return Err(KernelTimerError::InvalidDeadline);
        }
        Ok(Self(now))
    }

    pub fn from_nanos(nanos: u64) -> Result<Self, KernelTimerError> {
        Self::from_duration(Duration::from_nanos(nanos))
    }

    pub const fn as_duration(self) -> Duration {
        self.0
    }

    pub fn reached(self, deadline: MonotonicDeadline) -> bool {
        self.0 >= deadline.0
    }
}

/// Errors from the shared host kernel-timer service.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum KernelTimerError {
    #[error("kernel timer registration and cancellation are unavailable in hard IRQ context")]
    UnsafeContext,
    #[error("timer deadline must be finite")]
    InvalidDeadline,
    #[error("kernel timer identity space is exhausted")]
    GenerationExhausted,
    #[error("CPU {cpu_id} has no initialized timer base")]
    CpuUnavailable { cpu_id: usize },
    #[error("kernel timer capacity is exhausted on CPU {cpu_id}")]
    Capacity { cpu_id: usize },
    #[error("kernel timer handle is stale")]
    StaleHandle,
    #[error("kernel timer owner mismatch: expected CPU {expected}, current CPU {actual}")]
    OwnerMismatch { expected: usize, actual: usize },
}

/// Callback executed by the owner CPU's `ktimers/%u` service thread.
pub type KernelTimerCallback = Box<dyn FnOnce(MonotonicInstant) + Send + 'static>;

/// Callback for a stable timer registration that may restart itself.
pub type RestartableKernelTimerCallback =
    Box<dyn FnMut(MonotonicInstant) -> KernelTimerAction + Send + 'static>;
/// Owned callback for an explicitly hard-expiry kernel timer.
pub type HardRestartableKernelTimerCallback =
    Box<dyn FnMut(MonotonicInstant) -> HardKernelTimerAction + Send + 'static>;

/// Explicit capability for a bounded callback that may execute in hard IRQ.
///
/// The callback allocation is created and destroyed in task context. The
/// timer base invokes it without allocating, freeing, sleeping, performing a
/// registry lookup, or holding the deadline-base lock. Completion is moved to
/// `ktimers/%u` before the callback payload can be dropped.
pub struct HardKernelTimerCallback {
    callback: HardRestartableKernelTimerCallback,
}

impl HardKernelTimerCallback {
    /// Creates one hard-expiry callback capability.
    ///
    /// # Safety
    ///
    /// Every invocation must be bounded, non-panicking, allocation-free and
    /// valid in hard IRQ context. It must use only IRQ-safe synchronization
    /// and prebound capabilities; it may not sleep, perform registry lookup,
    /// invoke an untyped external callback, or clone/drop owning references.
    pub unsafe fn new(callback: HardRestartableKernelTimerCallback) -> Self {
        Self { callback }
    }

    fn invoke(&mut self, expired_at: MonotonicInstant) -> HardKernelTimerAction {
        (self.callback)(expired_at)
    }
}

/// Result returned by an explicitly hard-expiry callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HardKernelTimerAction {
    /// Destroy this registration after task-context reclamation.
    Complete,
    /// Keep the stable registration inactive until task context arms it again.
    Disarm,
    /// Reinsert the same registration at a new absolute deadline.
    Rearm(MonotonicDeadline),
}

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
    owner: TimerCpuId,
    identity: NonZeroU64,
}

impl KernelTimerHandle {
    pub(crate) const fn new(owner: TimerCpuId, identity: NonZeroU64) -> Self {
        Self { owner, identity }
    }

    /// Returns the CPU deadline base that owns this registration.
    pub const fn owner(self) -> TimerCpuId {
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

pub(crate) enum KernelTimerQueueCancel {
    Cancelled(KernelTimerEntry),
    Executing,
    Stale,
}

pub(crate) struct KernelTimerEntry {
    identity: NonZeroU64,
    deadline: Option<MonotonicDeadline>,
    expired_at: Option<MonotonicInstant>,
    callback: KernelTimerCallbackState,
}

enum KernelTimerCallbackState {
    OneShot(Option<KernelTimerCallback>),
    Restartable(RestartableKernelTimerCallback),
    HardRestartable(HardKernelTimerCallback),
}

impl KernelTimerEntry {
    pub(crate) fn new(
        deadline: MonotonicDeadline,
        callback: KernelTimerCallback,
    ) -> Result<Self, KernelTimerError> {
        Ok(Self {
            identity: next_kernel_timer_identity()?,
            deadline: Some(deadline),
            expired_at: None,
            callback: KernelTimerCallbackState::OneShot(Some(callback)),
        })
    }

    pub(crate) fn new_restartable(
        deadline: MonotonicDeadline,
        callback: RestartableKernelTimerCallback,
    ) -> Result<Self, KernelTimerError> {
        Ok(Self {
            identity: next_kernel_timer_identity()?,
            deadline: Some(deadline),
            expired_at: None,
            callback: KernelTimerCallbackState::Restartable(callback),
        })
    }

    pub(crate) fn new_hard_restartable(
        deadline: MonotonicDeadline,
        callback: HardKernelTimerCallback,
    ) -> Result<Self, KernelTimerError> {
        Ok(Self {
            identity: next_kernel_timer_identity()?,
            deadline: Some(deadline),
            expired_at: None,
            callback: KernelTimerCallbackState::HardRestartable(callback),
        })
    }

    fn deadline(&self) -> MonotonicDeadline {
        self.deadline
            .expect("only an armed kernel timer has a deadline")
    }

    pub(crate) const fn deadline_for_registration(&self) -> Option<MonotonicDeadline> {
        self.deadline
    }

    const fn identity(&self) -> NonZeroU64 {
        self.identity
    }

    fn expire(&mut self, now: MonotonicInstant) {
        assert!(self.expired_at.replace(now).is_none());
    }

    fn rearm(&mut self, deadline: MonotonicDeadline) {
        self.deadline = Some(deadline);
        self.expired_at = None;
    }

    fn disarm(&mut self) -> MonotonicDeadline {
        self.expired_at = None;
        self.deadline
            .take()
            .expect("only an armed kernel timer can be disarmed")
    }

    const fn is_armed(&self) -> bool {
        self.deadline.is_some()
    }

    const fn is_hard(&self) -> bool {
        matches!(self.callback, KernelTimerCallbackState::HardRestartable(_))
    }
}

fn next_kernel_timer_identity() -> Result<NonZeroU64, KernelTimerError> {
    let identity = NEXT_KERNEL_TIMER_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| KernelTimerError::GenerationExhausted)?;
    NonZeroU64::new(identity).ok_or(KernelTimerError::GenerationExhausted)
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
    pub(crate) fn invoke_soft(&mut self) -> KernelTimerAction {
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
            KernelTimerCallbackState::HardRestartable(_) => {
                panic!("hard kernel timer must not execute in ktimers/%u")
            }
        }
    }

    /// Invokes an explicitly hard-IRQ-safe callback.
    ///
    /// # Safety
    ///
    /// The caller must own the CPU's hard-timer execution context with local
    /// IRQs excluded. The deadline-base lock must not be held.
    pub(crate) unsafe fn invoke_hard(&mut self) -> HardKernelTimerAction {
        let expired_at = self
            .entry
            .expired_at
            .expect("claimed hard kernel timer must have an expiry sample");
        match &mut self.entry.callback {
            KernelTimerCallbackState::HardRestartable(callback) => callback.invoke(expired_at),
            KernelTimerCallbackState::OneShot(_) | KernelTimerCallbackState::Restartable(_) => {
                panic!("task-context kernel timer must not execute in hard IRQ")
            }
        }
    }

    const fn is_hard(&self) -> bool {
        self.entry.is_hard()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExecutingKernelTimer {
    identity: NonZeroU64,
    disposition: ExecutingKernelTimerDisposition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutingKernelTimerDisposition {
    Continue,
    Disarm,
    Rearm(MonotonicDeadline),
    Destroy,
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
    inactive: Vec<KernelTimerEntry>,
    expired: Vec<KernelTimerEntry>,
    executing: Vec<ExecutingKernelTimer>,
    completed: Vec<KernelTimerEntry>,
    capacity: usize,
}

impl KernelTimerQueue {
    pub(crate) const fn new(capacity: usize) -> Self {
        Self {
            active: Vec::new(),
            inactive: Vec::new(),
            expired: Vec::new(),
            executing: Vec::new(),
            completed: Vec::new(),
            capacity,
        }
    }

    /// Reserves every transition queue before timer IRQs can move entries.
    pub(crate) fn reserve_transition_capacity(
        &mut self,
        cpu_id: usize,
    ) -> Result<(), KernelTimerError> {
        self.active
            .try_reserve_exact(self.capacity)
            .map_err(|_| KernelTimerError::Capacity { cpu_id })?;
        self.inactive
            .try_reserve_exact(self.capacity)
            .map_err(|_| KernelTimerError::Capacity { cpu_id })?;
        self.expired
            .try_reserve_exact(self.capacity)
            .map_err(|_| KernelTimerError::Capacity { cpu_id })?;
        self.executing
            .try_reserve_exact(self.capacity)
            .map_err(|_| KernelTimerError::Capacity { cpu_id })?;
        self.completed
            .try_reserve_exact(self.capacity)
            .map_err(|_| KernelTimerError::Capacity { cpu_id })?;
        Ok(())
    }

    pub(crate) fn insert(
        &mut self,
        owner: TimerCpuId,
        entry: KernelTimerEntry,
    ) -> Result<KernelTimerHandle, KernelTimerEntry> {
        if self.active.len()
            + self.inactive.len()
            + self.expired.len()
            + self.executing.len()
            + self.completed.len()
            >= self.capacity
        {
            return Err(entry);
        }
        let handle = KernelTimerHandle::new(owner, entry.identity());
        self.insert_entry(entry);
        Ok(handle)
    }

    pub(crate) fn cancel(&mut self, handle: KernelTimerHandle) -> KernelTimerQueueCancel {
        if let Some(index) = self
            .active
            .iter()
            .position(|entry| entry.identity() == handle.identity())
        {
            return KernelTimerQueueCancel::Cancelled(self.active.remove(index));
        }
        if let Some(index) = self
            .inactive
            .iter()
            .position(|entry| entry.identity() == handle.identity())
        {
            return KernelTimerQueueCancel::Cancelled(self.inactive.remove(index));
        }
        let removed = self
            .expired
            .iter()
            .position(|entry| entry.identity() == handle.identity())
            .map(|index| self.expired.remove(index));
        if let Some(entry) = removed {
            return KernelTimerQueueCancel::Cancelled(entry);
        }
        if let Some(executing) = self
            .executing
            .iter_mut()
            .find(|entry| entry.identity == handle.identity())
        {
            executing.disposition = ExecutingKernelTimerDisposition::Destroy;
            return KernelTimerQueueCancel::Executing;
        }
        KernelTimerQueueCancel::Stale
    }

    pub(crate) fn arm_hard(
        &mut self,
        handle: KernelTimerHandle,
        deadline: MonotonicDeadline,
    ) -> bool {
        if let Some(index) = self
            .inactive
            .iter()
            .position(|entry| entry.identity() == handle.identity() && entry.is_hard())
        {
            let mut entry = self.inactive.remove(index);
            entry.rearm(deadline);
            self.insert_at(entry);
            return true;
        }
        if let Some(executing) = self
            .executing
            .iter_mut()
            .find(|entry| entry.identity == handle.identity())
            && executing.disposition != ExecutingKernelTimerDisposition::Destroy
        {
            // Like hrtimer_start() racing a running callback, task context
            // publishes the next arm on the stable identity. Completion owns
            // the only transition back into the active base.
            executing.disposition = ExecutingKernelTimerDisposition::Rearm(deadline);
            return true;
        }
        false
    }

    /// Disarms one stable hard registration without releasing its payload.
    ///
    /// `Some(Some(deadline))` reports an active entry that moved to inactive,
    /// `Some(None)` reports an already inactive or executing entry, and `None`
    /// reports a stale or non-hard handle.
    pub(crate) fn disarm_hard(
        &mut self,
        handle: KernelTimerHandle,
    ) -> Option<Option<MonotonicDeadline>> {
        if self
            .inactive
            .iter()
            .any(|entry| entry.identity() == handle.identity() && entry.is_hard())
        {
            return Some(None);
        }
        if let Some(index) = self
            .active
            .iter()
            .position(|entry| entry.identity() == handle.identity() && entry.is_hard())
        {
            let mut entry = self.active.remove(index);
            let deadline = entry.disarm();
            self.inactive.push(entry);
            return Some(Some(deadline));
        }
        if let Some(executing) = self
            .executing
            .iter_mut()
            .find(|entry| entry.identity == handle.identity())
        {
            if executing.disposition != ExecutingKernelTimerDisposition::Destroy {
                executing.disposition = ExecutingKernelTimerDisposition::Disarm;
            }
            return Some(None);
        }
        None
    }

    pub(crate) fn expire_due_soft(
        &mut self,
        now: MonotonicInstant,
        budget: usize,
    ) -> KernelTimerExpireBatch {
        let mut expired = 0;
        while expired < budget {
            let Some(index) = self.next_active_index(false) else {
                break;
            };
            if !now.reached(self.active[index].deadline()) {
                break;
            }
            let mut entry = self.active.remove(index);
            entry.expire(now);
            self.expired.push(entry);
            expired += 1;
        }
        KernelTimerExpireBatch {
            expired,
            pending: self.has_due_soft(now),
        }
    }

    pub(crate) fn claim_due_hard(&mut self, now: MonotonicInstant) -> Option<KernelTimerExecution> {
        let index = self.next_active_index(true)?;
        if !now.reached(self.active[index].deadline()) {
            return None;
        }
        let mut entry = self.active.remove(index);
        entry.expire(now);
        self.executing.push(ExecutingKernelTimer {
            identity: entry.identity(),
            disposition: ExecutingKernelTimerDisposition::Continue,
        });
        Some(KernelTimerExecution { entry })
    }

    pub(crate) fn claim_expired(&mut self) -> Option<KernelTimerExecution> {
        if self.expired.is_empty() {
            return None;
        }
        let entry = self.expired.remove(0);
        self.executing.push(ExecutingKernelTimer {
            identity: entry.identity(),
            disposition: ExecutingKernelTimerDisposition::Continue,
        });
        Some(KernelTimerExecution { entry })
    }

    pub(crate) fn complete_soft_execution(
        &mut self,
        mut execution: KernelTimerExecution,
        action: KernelTimerAction,
    ) -> Option<KernelTimerEntry> {
        assert!(!execution.is_hard());
        let position = self
            .executing
            .iter()
            .position(|entry| entry.identity == execution.entry.identity())
            .expect("completed kernel timer must remain in executing state");
        let executing = self.executing.swap_remove(position);
        if executing.disposition == ExecutingKernelTimerDisposition::Continue
            && let KernelTimerAction::Rearm(deadline) = action
        {
            execution.entry.rearm(deadline);
            self.insert_at(execution.entry);
            return None;
        }
        Some(execution.entry)
    }

    /// Completes one hard callback without dropping its payload in hard IRQ.
    ///
    /// Returns `true` when task-context reclamation was queued.
    pub(crate) fn complete_hard_execution(
        &mut self,
        mut execution: KernelTimerExecution,
        action: HardKernelTimerAction,
    ) -> bool {
        assert!(execution.is_hard());
        let position = self
            .executing
            .iter()
            .position(|entry| entry.identity == execution.entry.identity())
            .expect("completed hard kernel timer must remain in executing state");
        let executing = self.executing.swap_remove(position);
        match (executing.disposition, action) {
            (ExecutingKernelTimerDisposition::Destroy, _) => {
                self.completed.push(execution.entry);
                true
            }
            (ExecutingKernelTimerDisposition::Disarm, _) => {
                execution.entry.disarm();
                self.inactive.push(execution.entry);
                false
            }
            (ExecutingKernelTimerDisposition::Rearm(deadline), _) => {
                execution.entry.rearm(deadline);
                self.insert_at(execution.entry);
                false
            }
            (ExecutingKernelTimerDisposition::Continue, HardKernelTimerAction::Complete) => {
                self.completed.push(execution.entry);
                true
            }
            (ExecutingKernelTimerDisposition::Continue, HardKernelTimerAction::Disarm) => {
                execution.entry.disarm();
                self.inactive.push(execution.entry);
                false
            }
            (ExecutingKernelTimerDisposition::Continue, HardKernelTimerAction::Rearm(deadline)) => {
                execution.entry.rearm(deadline);
                self.insert_at(execution.entry);
                false
            }
        }
    }

    pub(crate) fn claim_completed(&mut self) -> Option<KernelTimerEntry> {
        (!self.completed.is_empty()).then(|| self.completed.remove(0))
    }

    fn insert_at(&mut self, entry: KernelTimerEntry) {
        debug_assert!(entry.is_armed());
        let position = self.active.partition_point(|candidate| {
            (candidate.deadline(), candidate.identity()) > (entry.deadline(), entry.identity())
        });
        self.active.insert(position, entry);
    }

    fn insert_entry(&mut self, entry: KernelTimerEntry) {
        if entry.is_armed() {
            self.insert_at(entry);
        } else {
            self.inactive.push(entry);
        }
    }

    pub(crate) fn next_soft_deadline(&self) -> Option<MonotonicDeadline> {
        self.next_active_entry(false)
            .map(KernelTimerEntry::deadline)
    }

    pub(crate) fn next_hard_deadline(&self) -> Option<MonotonicDeadline> {
        self.next_active_entry(true).map(KernelTimerEntry::deadline)
    }

    pub(crate) fn has_due_soft(&self, now: MonotonicInstant) -> bool {
        self.next_soft_deadline()
            .is_some_and(|deadline| now.reached(deadline))
    }

    pub(crate) fn has_expired(&self) -> bool {
        !self.expired.is_empty()
    }

    pub(crate) fn has_completed(&self) -> bool {
        !self.completed.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn has_inactive(&self) -> bool {
        !self.inactive.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn has_active_work(&self) -> bool {
        !self.active.is_empty()
            || !self.expired.is_empty()
            || !self.executing.is_empty()
            || !self.completed.is_empty()
    }

    fn next_active_index(&self, hard: bool) -> Option<usize> {
        self.active
            .iter()
            .rposition(|entry| entry.is_hard() == hard)
    }

    fn next_active_entry(&self, hard: bool) -> Option<&KernelTimerEntry> {
        self.next_active_index(hard)
            .map(|index| &self.active[index])
    }
}

impl fmt::Debug for KernelTimerQueue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KernelTimerQueue")
            .field("active", &self.active)
            .field("inactive", &self.inactive)
            .field("expired", &self.expired)
            .field("executing", &self.executing)
            .field("completed", &self.completed)
            .field("capacity", &self.capacity)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec::Vec};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::sync::SpinLock;

    fn deadline(nanos: u64) -> MonotonicDeadline {
        MonotonicDeadline::from_nanos(nanos).unwrap()
    }

    fn instant(nanos: u64) -> MonotonicInstant {
        MonotonicInstant::from_nanos(nanos).unwrap()
    }

    #[test]
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
        let handle = queue.insert(TimerCpuId::new(0), entry).unwrap();

        assert_eq!(queue.expire_due_soft(instant(10), 1).expired(), 1);
        let mut execution = queue.claim_expired().unwrap();
        let action = execution.invoke_soft();
        assert!(queue.complete_soft_execution(execution, action).is_none());
        assert_eq!(queue.next_soft_deadline(), Some(deadline(20)));

        assert_eq!(queue.expire_due_soft(instant(20), 1).expired(), 1);
        let mut execution = queue.claim_expired().unwrap();
        let action = execution.invoke_soft();
        assert!(queue.complete_soft_execution(execution, action).is_none());
        assert_eq!(queue.next_soft_deadline(), Some(deadline(30)));
        assert_eq!(invocations.load(Ordering::Relaxed), 2);

        assert!(matches!(
            queue.cancel(handle),
            KernelTimerQueueCancel::Cancelled(_)
        ));
        assert!(!queue.has_active_work());
    }

    #[test]
    fn cancellation_during_callback_prevents_restart() {
        let entry = KernelTimerEntry::new_restartable(
            deadline(10),
            Box::new(|_| KernelTimerAction::Rearm(deadline(20))),
        )
        .unwrap();
        let mut queue = KernelTimerQueue::new(1);
        let handle = queue.insert(TimerCpuId::new(0), entry).unwrap();
        assert_eq!(queue.expire_due_soft(instant(10), 1).expired(), 1);
        let mut execution = queue.claim_expired().unwrap();

        assert!(matches!(
            queue.cancel(handle),
            KernelTimerQueueCancel::Executing
        ));
        let action = execution.invoke_soft();
        assert!(queue.complete_soft_execution(execution, action).is_some());
        assert!(!queue.has_active_work());
    }

    #[test]
    fn hard_completion_defers_callback_reclamation_to_task_context() {
        let invocations = Arc::new(AtomicUsize::new(0));
        let callback_invocations = Arc::clone(&invocations);
        let callback = unsafe {
            // SAFETY: this test callback performs one bounded atomic operation.
            HardKernelTimerCallback::new(Box::new(move |_| {
                callback_invocations.fetch_add(1, Ordering::Relaxed);
                HardKernelTimerAction::Complete
            }))
        };
        let entry = KernelTimerEntry::new_hard_restartable(deadline(10), callback).unwrap();
        let mut queue = KernelTimerQueue::new(1);
        let handle = queue.insert(TimerCpuId::new(0), entry).unwrap();

        let mut execution = queue.claim_due_hard(instant(10)).unwrap();
        let action = unsafe {
            // SAFETY: the pure queue test models the hard callback transaction.
            execution.invoke_hard()
        };
        assert!(queue.complete_hard_execution(execution, action));
        assert_eq!(invocations.load(Ordering::Relaxed), 1);
        assert!(queue.has_completed());
        assert!(matches!(
            queue.cancel(handle),
            KernelTimerQueueCancel::Stale
        ));

        drop(queue.claim_completed());
        assert!(!queue.has_active_work());
    }

    #[test]
    fn hard_disarm_retains_one_stable_registration_without_reaping() {
        let callback = unsafe {
            // SAFETY: this callback returns one constant action.
            HardKernelTimerCallback::new(Box::new(|_| HardKernelTimerAction::Disarm))
        };
        let entry = KernelTimerEntry::new_hard_restartable(deadline(10), callback).unwrap();
        let mut queue = KernelTimerQueue::new(1);
        let handle = queue.insert(TimerCpuId::new(0), entry).unwrap();

        let mut execution = queue.claim_due_hard(instant(10)).unwrap();
        let action = unsafe {
            // SAFETY: the pure queue test models the hard callback transaction.
            execution.invoke_hard()
        };
        assert!(!queue.complete_hard_execution(execution, action));
        assert!(queue.has_inactive());
        assert!(!queue.has_completed());

        assert!(queue.arm_hard(handle, deadline(20)));
        let mut execution = queue.claim_due_hard(instant(20)).unwrap();
        let action = unsafe {
            // SAFETY: the pure queue test models the second hard transaction.
            execution.invoke_hard()
        };
        assert!(!queue.complete_hard_execution(execution, action));
        assert!(queue.has_inactive());
        assert!(matches!(
            queue.cancel(handle),
            KernelTimerQueueCancel::Cancelled(_)
        ));
        assert!(!queue.has_active_work());
    }

    #[test]
    fn task_arm_while_hard_callback_runs_owns_the_next_deadline() {
        let callback = unsafe {
            // SAFETY: this callback returns one constant action.
            HardKernelTimerCallback::new(Box::new(|_| HardKernelTimerAction::Disarm))
        };
        let entry = KernelTimerEntry::new_hard_restartable(deadline(10), callback).unwrap();
        let mut queue = KernelTimerQueue::new(1);
        let handle = queue.insert(TimerCpuId::new(0), entry).unwrap();

        let mut execution = queue.claim_due_hard(instant(10)).unwrap();
        assert!(queue.arm_hard(handle, deadline(20)));
        let action = unsafe {
            // SAFETY: the pure queue test models the hard callback transaction.
            execution.invoke_hard()
        };
        assert!(!queue.complete_hard_execution(execution, action));
        assert_eq!(queue.next_hard_deadline(), Some(deadline(20)));
    }

    #[test]
    fn equal_deadlines_execute_in_registration_order() {
        let order = Arc::new(SpinLock::new(Vec::new()));
        let mut queue = KernelTimerQueue::new(3);
        for sequence in 0..3 {
            let callback_order = Arc::clone(&order);
            let entry = KernelTimerEntry::new(
                deadline(10),
                Box::new(move |_| callback_order.lock().push(sequence)),
            )
            .unwrap();
            queue.insert(TimerCpuId::new(0), entry).unwrap();
        }

        assert_eq!(queue.expire_due_soft(instant(10), 3).expired(), 3);
        for _ in 0..3 {
            let mut execution = queue.claim_expired().unwrap();
            let action = execution.invoke_soft();
            drop(queue.complete_soft_execution(execution, action));
        }
        assert_eq!(&*order.lock(), &[0, 1, 2]);
    }

    #[test]
    fn expiration_budget_reports_due_work_left_behind() {
        let mut queue = KernelTimerQueue::new(3);
        for _ in 0..3 {
            let entry = KernelTimerEntry::new(deadline(10), Box::new(|_| {})).unwrap();
            queue.insert(TimerCpuId::new(0), entry).unwrap();
        }

        let batch = queue.expire_due_soft(instant(10), 2);
        assert_eq!(batch.expired(), 2);
        assert!(batch.pending());
        assert_eq!(queue.next_soft_deadline(), Some(deadline(10)));
    }

    #[test]
    fn cancelling_the_head_advances_to_the_next_deadline() {
        let mut queue = KernelTimerQueue::new(2);
        let later = KernelTimerEntry::new(deadline(30), Box::new(|_| {})).unwrap();
        let earlier = KernelTimerEntry::new(deadline(10), Box::new(|_| {})).unwrap();
        queue.insert(TimerCpuId::new(0), later).unwrap();
        let earlier_handle = queue.insert(TimerCpuId::new(0), earlier).unwrap();

        assert_eq!(queue.next_soft_deadline(), Some(deadline(10)));
        assert!(matches!(
            queue.cancel(earlier_handle),
            KernelTimerQueueCancel::Cancelled(_)
        ));
        assert_eq!(queue.next_soft_deadline(), Some(deadline(30)));
        assert!(matches!(
            queue.cancel(earlier_handle),
            KernelTimerQueueCancel::Stale
        ));
    }

    #[test]
    fn an_early_hard_edge_does_not_claim_the_registration() {
        let callback = unsafe {
            // SAFETY: this callback returns one constant action.
            HardKernelTimerCallback::new(Box::new(|_| HardKernelTimerAction::Disarm))
        };
        let entry = KernelTimerEntry::new_hard_restartable(deadline(20), callback).unwrap();
        let mut queue = KernelTimerQueue::new(1);
        queue.insert(TimerCpuId::new(0), entry).unwrap();

        assert!(queue.claim_due_hard(instant(19)).is_none());
        assert_eq!(queue.next_hard_deadline(), Some(deadline(20)));
        assert!(queue.claim_due_hard(instant(20)).is_some());
    }
}
