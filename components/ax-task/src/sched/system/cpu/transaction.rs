//! Linux-style owner runqueue transaction.

use super::*;
use crate::{
    sched::{
        algorithm::{
            BalanceScan, EnqueueReason, FairEntity, LinkedRqTaskRef, PickTaskResult, PickedThread,
            QueuedThreadSnapshot, RtEligibility,
        },
        system::{
            task_system::{SwitchEndpoint, TaskSystem},
            thread_sched::{SchedulerPlacement, ThreadSchedCell, ThreadSchedState},
        },
    },
    thread::SchedulingUrgency,
};

/// Linux `task_current()` and `task_on_rq_queued()` facts sampled under one rq
/// lock.
///
/// `Queued { outgoing: true }` is the legal switch-handoff window where the
/// task has left `rq->curr` but still retains its `p->on_cpu` stack claim.
/// `DelayedFair` keeps Linux's distinct `on_rq && sched_delayed` state from
/// being mistaken for runnable queue membership by policy and PI updates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::sched::system) enum OwnerRqTaskState {
    Current,
    Queued { outgoing: bool },
    DelayedFair { outgoing: bool },
    Inactive,
}

impl OwnerRqTaskState {
    pub(in crate::sched::system) const fn is_current(self) -> bool {
        matches!(self, Self::Current)
    }

    pub(in crate::sched::system) const fn is_queued(self) -> bool {
        matches!(self, Self::Queued { .. })
    }

    pub(in crate::sched::system) const fn is_runnable(self) -> bool {
        matches!(self, Self::Current | Self::Queued { .. })
    }

    pub(in crate::sched::system) const fn is_delayed_fair(self) -> bool {
        matches!(self, Self::DelayedFair { .. })
    }
}

#[derive(Clone, Copy)]
pub(crate) enum OwnerRqEntry {
    IrqSave,
    SchedulerFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerRqContext {
    RuntimeIrqSave,
    SchedulerFrame,
    OfflineBootstrap,
}

impl OwnerRqEntry {
    /// Returns whether this entry still needs the runtime owner assertion.
    ///
    /// `SchedulerFrame` is constructed only after the runtime atomically
    /// validates and claims the IRQ-off scheduler baton. Revalidating it below
    /// every TaskSystem entry would repeat the same CPU-local state walk.
    pub(crate) const fn requires_owner_context_validation(self) -> bool {
        matches!(self, Self::IrqSave)
    }

    /// Locks task scheduler state under this rq entry's IRQ ownership model.
    ///
    /// # Safety
    ///
    /// `SchedulerFrame` requires an active IRQ-off runtime scheduler baton.
    pub(in crate::sched::system) unsafe fn lock_thread_sched(
        self,
        cell: &ThreadSchedCell,
    ) -> IrqTicketGuard<'_, ThreadSchedState> {
        match self {
            Self::IrqSave => cell.lock(),
            Self::SchedulerFrame => {
                // SAFETY: forwarded from this method's contract.
                unsafe { cell.lock_scheduler_frame() }
            }
        }
    }

    /// Begins the selected rq locking protocol.
    ///
    /// # Safety
    ///
    /// `SchedulerFrame` requires an active IRQ-off runtime scheduler baton.
    #[inline(always)]
    pub(crate) unsafe fn begin<'a>(
        self,
        system: &'a TaskSystem,
        remote: &'a CpuRemote,
    ) -> OwnerRqTxn<'a> {
        match self {
            Self::IrqSave => OwnerRqTxn::begin(system, remote),
            Self::SchedulerFrame => {
                // SAFETY: forwarded from this method's contract.
                unsafe { OwnerRqTxn::begin_scheduler(system, remote) }
            }
        }
    }
}

/// One owner-CPU runqueue critical section.
///
/// Construction disables local IRQs, locks the rq, and samples `rq->clock`
/// exactly once. Callers must use the retained wall/task pair for the complete
/// class transition instead of opening nested rq locks or sampling a second
/// clock value.
pub(crate) struct OwnerRqTxn<'a> {
    system: &'a TaskSystem,
    remote: &'a CpuRemote,
    run_queue: Option<IrqTicketGuard<'a, CpuRunQueueState>>,
    clock: RunQueueClockSnapshot,
    request: Option<SchedulerRequestClaim>,
    context: OwnerRqContext,
    finished: bool,
}

/// Raw owner-rq lock ownership inherited by an incoming switch tail.
///
/// Linux carries `rq->lock` through `switch_to()` and releases it only after
/// `finish_task(prev)` clears `prev->on_cpu`. This token represents the same
/// bounded ownership interval without retaining mutable access to rq state.
#[derive(Debug)]
pub(crate) struct RqSwitchBaton {
    owner: CpuId,
    _raw: RawTicketBaton<CpuRunQueueState>,
}

impl RqSwitchBaton {
    pub(crate) fn finish(self, owner: CpuId) -> Result<(), TaskError> {
        if self.owner != owner {
            return Err(TaskError::InvalidConfiguration);
        }
        drop(self);
        Ok(())
    }
}

impl<'a> OwnerRqTxn<'a> {
    fn run_queue(&self) -> &CpuRunQueueState {
        self.run_queue
            .as_ref()
            .expect("an unfinished rq transaction must retain its lock")
    }

    fn run_queue_mut(&mut self) -> &mut CpuRunQueueState {
        self.run_queue
            .as_mut()
            .expect("an unfinished rq transaction must retain its lock")
    }

    fn scheduler_queue_mut(&mut self) -> &mut RunQueue {
        self.run_queue_mut().owner_transaction_queue_mut()
    }

    pub(crate) fn begin(system: &'a TaskSystem, remote: &'a CpuRemote) -> Self {
        let mut run_queue = remote.lock_run_queue(RunQueueGuardSource::Transaction);
        let clock = run_queue.update_clock();
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::record_owner_rq_irqsave_transaction();
        Self {
            system,
            remote,
            run_queue: Some(run_queue),
            clock,
            request: None,
            context: OwnerRqContext::RuntimeIrqSave,
            finished: false,
        }
    }

    /// Begins an rq transaction below a task scheduler lock that already owns
    /// local IRQ exclusion.
    pub(crate) fn begin_nested(
        system: &'a TaskSystem,
        remote: &'a CpuRemote,
        irq_owner: &'a IrqOwner<'_>,
    ) -> Self {
        let mut run_queue = remote.lock_run_queue_nested(irq_owner);
        let clock = run_queue.update_clock();
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::record_owner_rq_irqsave_transaction();
        Self {
            system,
            remote,
            run_queue: Some(run_queue),
            clock,
            request: None,
            context: OwnerRqContext::RuntimeIrqSave,
            finished: false,
        }
    }

    /// Begins the transaction from `__schedule()`/IRQ-return context where the
    /// runtime scheduler frame already owns local IRQ exclusion.
    ///
    /// # Safety
    ///
    /// The scheduler IRQ-off baton must outlive this transaction.
    pub(crate) unsafe fn begin_scheduler(system: &'a TaskSystem, remote: &'a CpuRemote) -> Self {
        #[cfg(feature = "qperf-metrics")]
        let rq_lock_started_ns = task_runtime::monotonic_now().as_nanos();
        // SAFETY: forwarded from this constructor's contract.
        let mut run_queue = unsafe { remote.lock_run_queue_irq_disabled() };
        #[cfg(feature = "qperf-metrics")]
        let rq_lock_finished_ns = task_runtime::monotonic_now().as_nanos();
        let clock = run_queue.update_clock();
        #[cfg(feature = "qperf-metrics")]
        {
            let rq_clock_finished_ns = task_runtime::monotonic_now().as_nanos();
            crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
                13,
                rq_lock_started_ns,
                rq_lock_finished_ns,
            );
            crate::diagnostics::counters::qperf_record_switch_scheduler_detail(
                14,
                rq_lock_finished_ns,
                rq_clock_finished_ns,
            );
            crate::diagnostics::counters::record_owner_rq_scheduler_transaction();
        }
        Self {
            system,
            remote,
            run_queue: Some(run_queue),
            clock,
            request: None,
            context: OwnerRqContext::SchedulerFrame,
            finished: false,
        }
    }

    /// Begins the first rq transaction while its owner CPU is still offline.
    ///
    /// This is the `sched_init()` counterpart of [`Self::begin_scheduler`]:
    /// boot already owns raw IRQ exclusion and `PREEMPT_DISABLED`, but no
    /// runtime IRQ-exit service may run until rq/current/idle are published.
    ///
    /// # Safety
    ///
    /// The calling CPU must retain its offline boot ownership and local IRQs
    /// must remain disabled for the complete transaction.
    pub(crate) unsafe fn begin_bootstrap(system: &'a TaskSystem, remote: &'a CpuRemote) -> Self {
        // SAFETY: forwarded from this constructor's boot-owner contract.
        let mut run_queue = unsafe { remote.lock_run_queue_irq_disabled() };
        let clock = run_queue.update_clock();
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::record_owner_rq_bootstrap_transaction();
        Self {
            system,
            remote,
            run_queue: Some(run_queue),
            clock,
            request: None,
            context: OwnerRqContext::OfflineBootstrap,
            finished: false,
        }
    }

    pub(crate) const fn clock(&self) -> RunQueueClockSnapshot {
        self.clock
    }

    pub(crate) const fn owner(&self) -> CpuId {
        self.remote.owner()
    }

    pub(crate) fn current_runtime_deadline(&self) -> SchedulerRuntimeDeadline {
        self.run_queue().current_runtime_deadline()
    }

    pub(crate) fn scheduler_deadline_rq_observation(
        &self,
        cpu: &CpuLocal,
    ) -> SchedulerDeadlineRqObservation {
        assert_eq!(
            cpu.owner(),
            self.owner(),
            "scheduler deadline observation must use its owner CPU transaction"
        );
        cpu.scheduler_deadline_rq_observation_in_run_queue(self.run_queue())
    }

    pub(crate) fn claim_scheduler_request(
        &mut self,
        scope: SchedulerRequestScope,
    ) -> SchedulerRequestClaim {
        let claim = self.remote.claim_scheduler_request(scope);
        self.request = Some(self.request.map_or(claim, |current| current.merge(claim)));
        self.request
            .expect("scheduler request claim was just installed")
    }

    pub(crate) fn adopt_scheduler_request(&mut self, claim: SchedulerRequestClaim) {
        assert!(
            self.request.replace(claim).is_none(),
            "one rq transaction may adopt only one initial scheduler claim"
        );
    }

    pub(crate) fn merge_scheduler_request(
        &mut self,
        scope: SchedulerRequestScope,
    ) -> SchedulerRequestClaim {
        self.claim_scheduler_request(scope)
    }
}

impl Deref for OwnerRqTxn<'_> {
    type Target = CpuRunQueueState;

    fn deref(&self) -> &Self::Target {
        self.run_queue
            .as_ref()
            .expect("an unfinished rq transaction must retain its lock")
    }
}

impl Drop for OwnerRqTxn<'_> {
    fn drop(&mut self) {
        if !self.finished {
            task_runtime::fatal_invariant(0x5251_5458, self.remote.owner().as_u32() as usize);
        }
    }
}

mod observation;

mod membership;

mod selection;

mod accounting;

mod commit;
