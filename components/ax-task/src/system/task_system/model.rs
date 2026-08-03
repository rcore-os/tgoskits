//! Scheduler ownership model and bounded work accounting.

use super::*;

/// Failure returned by [`TaskSystem::reap_thread_handle`].
///
/// A failed registry transition returns ownership of the strong handle, keeping
/// the registry generation pinned while the caller handles the error.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct OwnedThreadReapError {
    error: TaskError,
    handle: ThreadHandle,
}

impl OwnedThreadReapError {
    pub(super) const fn new(error: TaskError, handle: ThreadHandle) -> Self {
        Self { error, handle }
    }

    /// Returns the underlying scheduler error.
    pub const fn task_error(&self) -> TaskError {
        self.error
    }

    /// Returns the still-valid handle.
    pub fn into_retry_handle(self) -> ThreadHandle {
        self.handle
    }
}

/// One bounded pass performed by the dedicated task-work service thread.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeferredTaskWorkBatch {
    pub(super) deadline_events: usize,
    pub(super) deadline_callbacks: usize,
    pub(super) scheduler_tick_events: usize,
    pub(super) scheduler_tick_callbacks: usize,
    pub(super) exit_callbacks: usize,
    pub(super) reaped_threads: usize,
    pub(super) coroutine_reclaims: usize,
    pub(super) address_space_reclaims: usize,
}

impl DeferredTaskWorkBatch {
    /// Returns the number of queue entries or resources consumed by this pass.
    pub const fn processed(self) -> usize {
        self.deadline_events
            + self.scheduler_tick_events
            + self.exit_callbacks
            + self.reaped_threads
            + self.coroutine_reclaims
            + self.address_space_reclaims
    }

    /// Returns the number of Deadline extension callbacks invoked.
    pub const fn deadline_callbacks(self) -> usize {
        self.deadline_callbacks
    }

    /// Returns the number of scheduler-tick extension callbacks invoked.
    pub const fn scheduler_tick_callbacks(self) -> usize {
        self.scheduler_tick_callbacks
    }

    /// Returns zero-reference coroutine allocations reclaimed after hard IRQ.
    pub const fn coroutine_reclaims(self) -> usize {
        self.coroutine_reclaims
    }

    /// Returns active-mm ownership tokens whose final CPU lease was released.
    pub const fn address_space_reclaims(self) -> usize {
        self.address_space_reclaims
    }

    /// Returns whether another pass should run before the worker parks.
    pub const fn made_progress(self) -> bool {
        self.processed() != 0
    }

    /// Returns whether this pass consumed the complete shared caller budget.
    pub const fn saturated(self, limit: usize) -> bool {
        let capped_limit = if limit < crate::DEFAULT_BATCH_LIMIT {
            limit
        } else {
            crate::DEFAULT_BATCH_LIMIT
        };
        capped_limit != 0 && self.processed() == capped_limit
    }
}

/// Complete OS-independent scheduler instance.
///
/// No instance is stored globally. A runtime owns one pinned `TaskSystem` and
/// passes explicit object references to the scheduler or exposes them through its
/// trait-FFI facade.
///
/// IRQ and remote producers wake through
/// [`ThreadWakeHandle::wake`](crate::ThreadWakeHandle::wake). The wake path
/// serializes thread state, selects an online destination, and activates the
/// thread under that destination's IRQ-safe runqueue lock. Owner-control
/// inboxes are reserved for migration, policy, and deferred owner work.
#[derive(Debug)]
pub struct TaskSystem {
    pub(super) config: TaskSystemConfig,
    pub(super) cpu_remotes: Vec<Arc<CpuRemote>>,
    // Cold-path order is registry/PI/admission -> root domain -> thread cell.
    // Wake and placement hot paths lock thread state before the target runqueue.
    pub(super) state: PreemptTicketLock<TaskSystemState>,
    pub(super) root_domain: PreemptTicketLock<RootDomainState>,
    pub(super) deferred_coroutine_reclaims: SchedulerInbox,
    pub(super) deferred_deadline_callbacks: SchedulerInbox,
    pub(super) deferred_scheduler_ticks: SchedulerInbox,
    pub(super) task_work: Arc<TaskWorkDoorbell>,
    pub(super) topology_sequence: SequenceCounter,
    pub(super) online_count: AtomicUsize,
    pub(super) pending_deadline_admission_release: AtomicU64,
}

#[derive(Debug)]
pub(super) struct RootDomainState {
    pub(super) online: CpuSet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BalanceReason {
    RtDeadlinePush,
    IdlePull,
    FairPeriodic,
}

/// Result of one opportunistic owner-to-owner balance attempt.
///
/// `Retry` means the transfer transaction observed a concurrent affinity,
/// hotplug, or publication change and restored every local ownership record.
/// It is not a failure of the already committed local scheduling decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BalanceTransferOutcome {
    Migrated(ThreadId),
    NoCandidate,
    Retry,
}

impl BalanceTransferOutcome {
    pub(super) const fn migrated(self) -> Option<ThreadId> {
        match self {
            Self::Migrated(thread) => Some(thread),
            Self::NoCandidate | Self::Retry => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FairBalanceResult {
    Migrated(ThreadId),
    Balanced,
    Constrained,
}

impl FairBalanceResult {
    pub(super) const fn migrated(self) -> Option<ThreadId> {
        match self {
            Self::Migrated(thread) => Some(thread),
            Self::Balanced | Self::Constrained => None,
        }
    }
}

pub(super) const FAIR_BALANCE_BALANCED_BACKOFF_FACTOR: u64 = 2;
pub(super) const FAIR_BALANCE_CONSTRAINED_BACKOFF_FACTOR: u64 = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DeferredTaskWorkClass {
    Deadline,
    SchedulerTick,
    Exit,
    Reap,
    Reclaim,
}

impl DeferredTaskWorkClass {
    pub(super) const COUNT: usize = 5;

    pub(super) const fn next(self) -> Self {
        match self {
            Self::Deadline => Self::SchedulerTick,
            Self::SchedulerTick => Self::Exit,
            Self::Exit => Self::Reap,
            Self::Reap => Self::Reclaim,
            Self::Reclaim => Self::Deadline,
        }
    }
}

/// Owns the unprocessed suffix of one already-detached owner inbox batch.
///
/// `SchedulerInbox::drain` releases every intrusive node before the caller
/// interprets any message. If processing one message fails, this guard still
/// consumes every later raw `Arc` payload and its scheduler-delivery lease.
pub(super) struct DetachedOwnerMessageBatch<'batch> {
    pub(super) messages: &'batch [InboxMessage],
    pub(super) next: usize,
}

impl<'batch> DetachedOwnerMessageBatch<'batch> {
    pub(super) const fn new(messages: &'batch [InboxMessage]) -> Self {
        Self { messages, next: 0 }
    }

    pub(super) fn next(&mut self) -> Option<InboxMessage> {
        let message = self.messages.get(self.next).copied()?;
        self.next += 1;
        Some(message)
    }

    pub(super) fn release(message: InboxMessage) {
        if message.payload() == 0 {
            return;
        }
        let core = unsafe {
            // SAFETY: every non-zero owner message transfers exactly one
            // `ThreadCore` Arc count into its payload. This detached batch owns
            // that count even when normal message processing aborts early.
            Arc::from_raw(ptr::with_exposed_provenance::<ThreadCore>(
                message.payload(),
            ))
        };
        let _delivery = core.accept_scheduler_inbox_delivery();
    }
}

impl Drop for DetachedOwnerMessageBatch<'_> {
    fn drop(&mut self) {
        for &message in &self.messages[self.next..] {
            Self::release(message);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FairPolicyPlacement {
    pub(super) source_virtual_time: u64,
    pub(super) destination_virtual_time: u64,
}
