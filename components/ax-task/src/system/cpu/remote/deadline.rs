//! IRQ-safe per-CPU task-deadline base.
//!
//! Linux hrtimer bases are per CPU, but their raw lock is reachable from a
//! remote `task_rq_lock()` migration transaction. Keeping this state below
//! [`CpuRemote`] provides the same ownership boundary: the local timer IRQ and
//! soft-timer worker remain the only consumers, while a remote wake migration
//! may cancel an old registration before moving its rq bandwidth.

use super::*;

const DEADLINE_SNAPSHOT_NONE: u64 = 1 << 63;
const DEADLINE_SNAPSHOT_UNINITIALIZED: u64 = u64::MAX;

fn encode_deadline_snapshot(deadline: Option<MonotonicDeadline>) -> u64 {
    deadline.map_or(DEADLINE_SNAPSHOT_NONE, MonotonicDeadline::as_nanos)
}

fn decode_deadline_snapshot(snapshot: u64) -> Option<MonotonicDeadline> {
    (snapshot != DEADLINE_SNAPSHOT_NONE)
        .then(|| MonotonicDeadline::from_nanos(snapshot))
        .flatten()
}

/// Typed reason for entering the per-CPU task-deadline base.
///
/// Each reason maps to one qperf IRQ-ticket category, so the aggregate keeps
/// one counter update per existing lock acquisition rather than adding a
/// second diagnostic operation to the hot path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeadlineBaseGuardSource {
    Publication,
    Registration,
    HardExpiry,
    SoftExpiry,
    Lifecycle,
}

impl DeadlineBaseGuardSource {
    pub(crate) const fn irq_guard_source(self) -> crate::runtime::IrqGuardSource {
        match self {
            Self::Publication => crate::runtime::IrqGuardSource::CpuDeadlinePublicationTicket,
            Self::Registration => crate::runtime::IrqGuardSource::CpuDeadlineRegistrationTicket,
            Self::HardExpiry => crate::runtime::IrqGuardSource::CpuDeadlineHardExpiryTicket,
            Self::SoftExpiry => crate::runtime::IrqGuardSource::CpuDeadlineSoftExpiryTicket,
            Self::Lifecycle => crate::runtime::IrqGuardSource::CpuDeadlineLifecycleTicket,
        }
    }
}

/// One authoritative per-CPU deadline base plus its empty-base publication.
///
/// `active` is derived from state protected by `state`; it does not own timer
/// identity or expiry progress. Writers publish the derived bit before
/// releasing the state lock, while readers use it only to reject a definitely
/// empty base without entering an IRQ-disabled critical section.
#[derive(Debug)]
pub(crate) struct CpuDeadlineBase {
    state: IrqTicketLock<CpuDeadlineState>,
    active: AtomicBool,
    snapshot: CpuDeadlineSnapshot,
}

/// Coherent lock-free projection of the authoritative deadline base.
///
/// Writers are serialized by `state`. The sequence makes the timer head and
/// its publication one observation so the owner fast path cannot combine two
/// different base generations.
#[derive(Debug)]
struct CpuDeadlineSnapshot {
    sequence: AtomicU64,
    timer_deadline: AtomicU64,
    publication_deadline: AtomicU64,
    publication_runtime_deadline: AtomicU64,
}

#[derive(Clone, Copy)]
struct CpuDeadlineSnapshotValue {
    timer_deadline: Option<MonotonicDeadline>,
    publication: Option<SchedulerDeadlinePublicationState>,
}

impl CpuDeadlineSnapshot {
    const fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            timer_deadline: AtomicU64::new(DEADLINE_SNAPSHOT_NONE),
            publication_deadline: AtomicU64::new(DEADLINE_SNAPSHOT_UNINITIALIZED),
            publication_runtime_deadline: AtomicU64::new(DEADLINE_SNAPSHOT_NONE),
        }
    }

    fn publish(&self, state: &CpuDeadlineState) {
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel);
        self.timer_deadline.store(
            encode_deadline_snapshot(state.timer_deadline()),
            Ordering::Relaxed,
        );
        self.publication_deadline.store(
            state
                .publication
                .map_or(DEADLINE_SNAPSHOT_UNINITIALIZED, |publication| {
                    encode_deadline_snapshot(publication.deadline)
                }),
            Ordering::Relaxed,
        );
        self.publication_runtime_deadline.store(
            state.publication.map_or(DEADLINE_SNAPSHOT_NONE, |publication| {
                encode_deadline_snapshot(publication.runtime_deadline)
            }),
            Ordering::Relaxed,
        );
        self.sequence
            .store(sequence.wrapping_add(2), Ordering::Release);
    }

    fn read(&self) -> CpuDeadlineSnapshotValue {
        loop {
            let before = self.sequence.load(Ordering::Acquire);
            if before & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let timer_deadline = self.timer_deadline.load(Ordering::Relaxed);
            let publication_deadline = self.publication_deadline.load(Ordering::Relaxed);
            let publication_runtime_deadline =
                self.publication_runtime_deadline.load(Ordering::Relaxed);
            let after = self.sequence.load(Ordering::Acquire);
            if before == after {
                return CpuDeadlineSnapshotValue {
                    timer_deadline: decode_deadline_snapshot(timer_deadline),
                    publication: (publication_deadline != DEADLINE_SNAPSHOT_UNINITIALIZED).then(
                        || SchedulerDeadlinePublicationState {
                            deadline: decode_deadline_snapshot(publication_deadline),
                            runtime_deadline: decode_deadline_snapshot(
                                publication_runtime_deadline,
                            ),
                        },
                    ),
                };
            }
        }
    }
}

pub(crate) struct CpuDeadlineReadGuard<'a> {
    state: IrqTicketGuard<'a, CpuDeadlineState>,
}

impl core::ops::Deref for CpuDeadlineReadGuard<'_> {
    type Target = CpuDeadlineState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

pub(crate) struct CpuDeadlineActivityGuard<'a> {
    state: IrqTicketGuard<'a, CpuDeadlineState>,
    base: &'a CpuDeadlineBase,
}

pub(crate) struct CpuDeadlinePublicationGuard<'a> {
    state: IrqTicketGuard<'a, CpuDeadlineState>,
    base: &'a CpuDeadlineBase,
}

impl core::ops::Deref for CpuDeadlineActivityGuard<'_> {
    type Target = CpuDeadlineState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl core::ops::DerefMut for CpuDeadlineActivityGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

impl Drop for CpuDeadlineActivityGuard<'_> {
    fn drop(&mut self) {
        self.base.publish_activity_snapshot(&self.state);
    }
}

impl core::ops::Deref for CpuDeadlinePublicationGuard<'_> {
    type Target = CpuDeadlineState;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl core::ops::DerefMut for CpuDeadlinePublicationGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

impl Drop for CpuDeadlinePublicationGuard<'_> {
    fn drop(&mut self) {
        self.base.snapshot.publish(&self.state);
    }
}

impl CpuDeadlineBase {
    pub(crate) fn new(config: TaskSystemConfig) -> Self {
        Self {
            state: IrqTicketLock::new(CpuDeadlineState::new(config)),
            active: AtomicBool::new(false),
            snapshot: CpuDeadlineSnapshot::new(),
        }
    }

    pub(crate) fn read(&self, source: DeadlineBaseGuardSource) -> CpuDeadlineReadGuard<'_> {
        CpuDeadlineReadGuard {
            state: self.state.lock(source.irq_guard_source()),
        }
    }

    pub(crate) fn read_if_active(
        &self,
        source: DeadlineBaseGuardSource,
    ) -> Option<CpuDeadlineReadGuard<'_>> {
        self.active
            .load(Ordering::Acquire)
            .then(|| self.read(source))
    }

    pub(crate) fn lock_publication(&self) -> CpuDeadlinePublicationGuard<'_> {
        CpuDeadlinePublicationGuard {
            state: self
                .state
                .lock(DeadlineBaseGuardSource::Publication.irq_guard_source()),
            base: self,
        }
    }

    pub(crate) fn lock_activity(
        &self,
        source: DeadlineBaseGuardSource,
    ) -> CpuDeadlineActivityGuard<'_> {
        CpuDeadlineActivityGuard {
            state: self.state.lock(source.irq_guard_source()),
            base: self,
        }
    }

    pub(crate) fn lock_activity_if_active(
        &self,
        source: DeadlineBaseGuardSource,
    ) -> Option<CpuDeadlineActivityGuard<'_>> {
        self.active
            .load(Ordering::Acquire)
            .then(|| self.lock_activity(source))
    }

    fn publish_activity_snapshot(&self, state: &CpuDeadlineState) {
        self.snapshot.publish(state);
        self.active
            .store(state.has_active_work(), Ordering::Release);
    }

    pub(crate) fn publication_snapshot_matches(
        &self,
        non_timer: SchedulerNonTimerDeadlines,
    ) -> bool {
        let snapshot = self.snapshot.read();
        let deadline = [snapshot.timer_deadline, non_timer.deadline]
            .into_iter()
            .flatten()
            .min();
        snapshot.publication
            == Some(SchedulerDeadlinePublicationState {
                deadline,
                runtime_deadline: non_timer.runtime_deadline,
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchedulerNonTimerDeadlines {
    pub(crate) deadline: Option<MonotonicDeadline>,
    pub(crate) runtime_deadline: Option<MonotonicDeadline>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchedulerDeadlinePublicationState {
    pub(crate) deadline: Option<MonotonicDeadline>,
    pub(crate) runtime_deadline: Option<MonotonicDeadline>,
}

#[derive(Debug)]
pub(crate) struct CpuDeadlineState {
    pub(crate) queue: TaskDeadlineQueue,
    pub(crate) kernel_timers: KernelTimerQueue,
    pub(crate) expired_buffer: Vec<ExpiredTaskDeadline>,
    pub(crate) expired_count: usize,
    claimed_task_expiration: Option<ClaimedTaskExpiration>,
    last_service_claim_was_kernel: bool,
    /// Mirrors Linux `hrtimer_cpu_base::softirq_activated`.
    ///
    /// A due queue head does not set this bit. Only the hard clockevent path
    /// may transfer progress ownership to `ktimers/%u`; the worker clears the
    /// bit after draining every due and buffered soft expiry.
    pub(crate) softirq_activated: bool,
    pub(crate) generation: u64,
    pub(crate) publication: Option<SchedulerDeadlinePublicationState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KtimerClaimClass {
    Kernel,
    Task,
}

impl CpuDeadlineState {
    pub(crate) fn new(config: TaskSystemConfig) -> Self {
        Self {
            queue: TaskDeadlineQueue::new(config.thread_capacity()),
            kernel_timers: KernelTimerQueue::new(config.thread_capacity()),
            expired_buffer: vec![ExpiredTaskDeadline::EMPTY; config.batch_limit()],
            expired_count: 0,
            claimed_task_expiration: None,
            last_service_claim_was_kernel: false,
            softirq_activated: false,
            generation: 0,
            publication: None,
        }
    }

    pub(crate) fn has_active_work(&self) -> bool {
        !self.queue.is_empty()
            || self.kernel_timers.has_active_work()
            || self.expired_count != 0
            || self.claimed_task_expiration.is_some()
            || self.softirq_activated
    }

    pub(crate) fn timer_deadline(&self) -> Option<MonotonicDeadline> {
        let hard = [
            self.queue.next_scheduler_deadline(),
            self.kernel_timers.next_hard_deadline(),
        ]
        .into_iter()
        .flatten()
        .min();
        let soft = (!self.softirq_activated)
            .then(|| {
                [
                    self.queue.next_soft_deadline(),
                    self.kernel_timers.next_soft_deadline(),
                ]
                .into_iter()
                .flatten()
                .min()
            })
            .flatten();
        [hard, soft].into_iter().flatten().min()
    }

    pub(crate) fn claim_next_buffered_expiration(&mut self) -> Option<ExpiredTaskDeadline> {
        assert!(
            self.claimed_task_expiration.is_none(),
            "one ktimer worker may own only one task expiration"
        );
        let event = self.expired_buffer[..self.expired_count].first().copied()?;
        let event = self
            .take_buffered_event(event)
            .expect("the selected task expiration must remain buffered");
        self.claimed_task_expiration = Some(ClaimedTaskExpiration {
            event,
            cancel_requested: false,
        });
        Some(event)
    }

    pub(crate) fn complete_claimed_task_expiration(
        &mut self,
        event: ExpiredTaskDeadline,
    ) -> Option<bool> {
        let claimed = self.claimed_task_expiration.take()?;
        if claimed.event != event {
            self.claimed_task_expiration = Some(claimed);
            return None;
        }
        Some(claimed.cancel_requested)
    }

    pub(crate) fn cancel_expired_task_deadline(
        &mut self,
        registration: &TaskDeadlineRegistration,
    ) -> bool {
        if self.take_buffered_expiration(registration).is_some() {
            return true;
        }
        let Some(claimed) = self.claimed_task_expiration.as_mut() else {
            return false;
        };
        if !expiration_matches_registration(claimed.event, registration) {
            return false;
        }
        claimed.cancel_requested = true;
        true
    }

    pub(crate) const fn has_claimed_task_expiration(&self) -> bool {
        self.claimed_task_expiration.is_some()
    }

    pub(crate) fn select_service_claim_class(
        &mut self,
        kernel_pending: bool,
        task_pending: bool,
    ) -> Option<KtimerClaimClass> {
        let claim_class = if kernel_pending {
            if !task_pending || !self.last_service_claim_was_kernel {
                KtimerClaimClass::Kernel
            } else {
                KtimerClaimClass::Task
            }
        } else if task_pending {
            KtimerClaimClass::Task
        } else {
            return None;
        };
        self.last_service_claim_was_kernel = claim_class == KtimerClaimClass::Kernel;
        Some(claim_class)
    }

    pub(crate) fn take_buffered_expiration(
        &mut self,
        registration: &TaskDeadlineRegistration,
    ) -> Option<ExpiredTaskDeadline> {
        self.take_buffered_expiration_if(|event| {
            expiration_matches_registration(event, registration)
        })
    }

    pub(crate) fn take_buffered_event(
        &mut self,
        event: ExpiredTaskDeadline,
    ) -> Option<ExpiredTaskDeadline> {
        self.take_buffered_expiration_if(|candidate| candidate == event)
    }

    fn take_buffered_expiration_if(
        &mut self,
        matches: impl Fn(ExpiredTaskDeadline) -> bool,
    ) -> Option<ExpiredTaskDeadline> {
        let index = self.expired_buffer[..self.expired_count]
            .iter()
            .copied()
            .position(matches)?;
        let event = self.expired_buffer[index];
        self.expired_buffer
            .copy_within(index + 1..self.expired_count, index);
        self.expired_count -= 1;
        self.expired_buffer[self.expired_count] = ExpiredTaskDeadline::EMPTY;
        Some(event)
    }
}

#[derive(Debug)]
struct ClaimedTaskExpiration {
    event: ExpiredTaskDeadline,
    cancel_requested: bool,
}

fn expiration_matches_registration(
    event: ExpiredTaskDeadline,
    registration: &TaskDeadlineRegistration,
) -> bool {
    event.thread() == Some(registration.thread())
        && event.token() == registration.token()
        && event.deadline() == Some(registration.deadline())
        && event.kind() == Some(registration.kind())
}

impl CpuRemote {
    pub(in crate::system::cpu) fn deadline_is_quiescent_for_offline(&self) -> bool {
        self.read_active_deadline_base(DeadlineBaseGuardSource::Lifecycle)
            .is_none_or(|deadlines| !deadlines.has_active_work())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timer::{TaskDeadlineKind, TaskDeadlineNode};

    #[test]
    fn activated_softirq_does_not_hide_scheduler_hard_deadline() {
        let mut state = CpuDeadlineState::new(TaskSystemConfig::new(1));
        let node = TaskDeadlineNode::deadline_cbs_for_thread(ThreadId::from_parts(1, 1));
        let deadline = MonotonicDeadline::from_nanos(10).unwrap();
        let _registration = state
            .queue
            .arm(&node, deadline, TaskDeadlineKind::DeadlineCbs)
            .unwrap();
        state.softirq_activated = true;

        assert_eq!(state.timer_deadline(), Some(deadline));
    }

    #[test]
    fn claimed_task_expiration_remains_cancel_visible_until_completion() {
        let mut state = CpuDeadlineState::new(TaskSystemConfig::new(1).with_batch_limit(1));
        let node = TaskDeadlineNode::for_thread(ThreadId::from_parts(1, 1));
        let registration = state
            .queue
            .arm(
                &node,
                MonotonicDeadline::from_nanos(10).unwrap(),
                TaskDeadlineKind::park_timeout(1),
            )
            .unwrap();
        let batch = state.queue.expire_soft(
            TaskDeadlineExpireRequest::new(MonotonicInstant::from_nanos(10).unwrap(), 1),
            &mut state.expired_buffer,
        );
        state.expired_count = batch.expired();

        let event = state.claim_next_buffered_expiration().unwrap();

        assert!(state.cancel_expired_task_deadline(&registration));
        assert_eq!(state.complete_claimed_task_expiration(event), Some(true));
        assert!(!state.has_claimed_task_expiration());
    }

    #[test]
    fn task_and_kernel_claims_alternate_when_both_remain_pending() {
        let mut state = CpuDeadlineState::new(TaskSystemConfig::new(1));

        assert_eq!(
            state.select_service_claim_class(true, true),
            Some(KtimerClaimClass::Kernel)
        );
        assert_eq!(
            state.select_service_claim_class(true, true),
            Some(KtimerClaimClass::Task)
        );
        assert_eq!(
            state.select_service_claim_class(true, true),
            Some(KtimerClaimClass::Kernel)
        );
    }
}
