use core::sync::atomic::fence;

use super::*;

const REQUEST_PREEMPT: u64 = 1 << 0;
const REQUEST_OWNER_WORK: u64 = 1 << 1;
const REQUEST_PREEMPT_LAZY: u64 = 1 << 2;
const REQUEST_REASON_MASK: u64 = REQUEST_PREEMPT | REQUEST_PREEMPT_LAZY | REQUEST_OWNER_WORK;
const REQUEST_IDLE_POLLING: u64 = 1 << 3;
const REQUEST_PARK_PREEMPT_DEFERRED: u64 = 1 << 4;
const REQUEST_PARK_PREEMPT_LAZY_DEFERRED: u64 = 1 << 5;
const DEFERRED_SCHEDULER_WORK_OFFLINE_INVARIANT: u32 = 0x4453_574f;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SchedulerRequestDelivery {
    /// The owner is in the idle polling protocol and will observe the sticky
    /// work bit before committing to sleep.
    PollingOwner,
    /// The runtime must notify the shared physical IPI delivery edge.
    ///
    /// The runtime transports only a coalescible edge. Logical ownership
    /// remains in the sticky request bits and the owner inbox, matching
    /// Linux's split between `TIF_NEED_RESCHED`/`wake_list` and the IPI.
    DoorbellRequired,
}

/// Linux PREEMPT_RT's two reschedule classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RescheduleKind {
    /// `TIF_NEED_RESCHED`: consumed by preempt-enable and IRQ return.
    Immediate,
    /// `TIF_NEED_RESCHED_LAZY`: consumed at an explicit scheduling point,
    /// return to userspace, or after the periodic tick promotes it.
    Lazy,
}

impl RescheduleKind {
    const fn request_bit(self) -> u64 {
        match self {
            Self::Immediate => REQUEST_PREEMPT,
            Self::Lazy => REQUEST_PREEMPT_LAZY,
        }
    }
}

/// Which logical preemption classes one scheduler entry may consume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SchedulerRequestScope {
    /// A kernel preempt-enable or IRQ-return safe point. Lazy Fair requests
    /// remain pending unless an ordinary request makes `__schedule()` run.
    Immediate,
    /// An explicit schedule/block/yield or return-to-userspace safe point.
    All,
}

impl SchedulerRequestScope {
    const fn claim_mask(self) -> u64 {
        match self {
            Self::Immediate => REQUEST_PREEMPT | REQUEST_OWNER_WORK,
            Self::All => REQUEST_REASON_MASK,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SchedulerRequestPublication {
    delivery: SchedulerRequestDelivery,
    #[cfg(feature = "task-test-hooks")]
    previous_owner_work: bool,
}

impl SchedulerRequestPublication {
    #[cfg(feature = "task-test-hooks")]
    pub(super) const fn previous_owner_work_requested(self) -> bool {
        self.previous_owner_work
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SchedulerRequestClaim {
    immediate_preempt: bool,
    lazy_preempt: bool,
}

impl SchedulerRequestClaim {
    pub(crate) const fn immediate_preempt_requested(self) -> bool {
        self.immediate_preempt
    }

    pub(crate) const fn lazy_preempt_requested(self) -> bool {
        self.lazy_preempt
    }

    pub(crate) const fn preemption_requested(self) -> bool {
        self.immediate_preempt || self.lazy_preempt
    }

    pub(crate) const fn merge(self, other: Self) -> Self {
        Self {
            immediate_preempt: self.immediate_preempt || other.immediate_preempt,
            lazy_preempt: self.lazy_preempt || other.lazy_preempt,
        }
    }
}

#[derive(Debug)]
pub(super) struct SchedulerRequestState {
    request: AtomicU64,
}

impl SchedulerRequestState {
    pub(super) const fn new() -> Self {
        Self {
            request: AtomicU64::new(0),
        }
    }
}

impl CpuRemote {
    pub(crate) fn is_scheduler_ready(&self) -> bool {
        // CPU online publication is ordered after bootstrap/current and idle
        // installation. Do not mirror `rq->curr` in an atomic readiness bit:
        // lifecycle plus the immutable idle identity are the stable facts
        // remote placement needs here.
        self.is_online() && self.idle_thread().is_some()
    }

    /// Publishes a sticky owner-CPU reschedule request.
    pub(crate) fn request_reschedule(&self, kind: RescheduleKind) {
        let Some(_publication) = self.begin_publication() else {
            return;
        };
        let _ = self.request_reschedule_owned(kind);
    }

    fn request_reschedule_owned(
        &self,
        kind: RescheduleKind,
    ) -> Option<SchedulerRequestPublication> {
        self.publish_scheduler_request_owned(kind.request_bit())
    }

    /// Publishes a remote preemption after the runqueue transaction is visible.
    ///
    /// Like Linux `__resched_curr()`, only an ordinary request rings a remote
    /// reschedule IPI. A lazy request remains a logical task flag; idle
    /// preemption is classified as ordinary while the target rq is locked.
    pub(crate) fn request_remote_reschedule(&self, kind: RescheduleKind) {
        let Some(_publication) = self.begin_owner_delivery() else {
            return;
        };
        let _irq = IrqScope::enter();
        if let Some(publication) = self.request_reschedule_owned(kind)
            && kind == RescheduleKind::Immediate
        {
            self.deliver_scheduler_work_owned(publication);
        }
    }

    /// Publishes coupled preemption and owner-work reasons before ringing one
    /// physical scheduler doorbell.
    ///
    /// One rq transaction may make both facts true. They share transport but
    /// remain separate sticky bits, matching Linux's rule that scheduler state
    /// and deferred work are visible before the IPI.
    pub(crate) fn request_remote_reschedule_with_scheduler_work(&self, kind: RescheduleKind) {
        let Some(_publication) = self.begin_owner_delivery() else {
            return;
        };
        let _irq = IrqScope::enter();
        if let Some(publication) =
            self.publish_scheduler_request_owned(kind.request_bit() | REQUEST_OWNER_WORK)
        {
            self.deliver_scheduler_work_owned(publication);
        }
    }

    pub(crate) fn request_scheduler_work(&self) {
        let _delivered = self.request_scheduler_work_delivery();
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn request_scheduler_work_for_test(&self) -> bool {
        self.request_scheduler_work_delivery()
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn request_scheduler_work_transition_for_test(&self) -> bool {
        let Some(_publication) = self.begin_owner_delivery() else {
            return false;
        };
        let _irq = IrqScope::enter();
        let Some(publication) = self.request_scheduler_work_owned() else {
            return false;
        };
        self.deliver_scheduler_work_owned(publication);
        true
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn request_combined_scheduler_work_transition_for_test(&self) -> bool {
        let Some(_publication) = self.begin_owner_delivery() else {
            return false;
        };
        let _irq = IrqScope::enter();
        let Some(publication) = self.publish_scheduler_request_owned(
            RescheduleKind::Immediate.request_bit() | REQUEST_OWNER_WORK,
        ) else {
            return false;
        };
        self.deliver_scheduler_work_owned(publication);
        true
    }

    fn request_scheduler_work_delivery(&self) -> bool {
        let Some(_publication) = self.begin_owner_delivery() else {
            return false;
        };
        let _irq = IrqScope::enter();
        self.request_scheduler_work_owned()
            .is_none_or(|publication| self.deliver_scheduler_work_owned(publication))
    }

    pub(super) fn request_scheduler_work_owned(&self) -> Option<SchedulerRequestPublication> {
        self.publish_scheduler_request_owned(REQUEST_OWNER_WORK)
    }

    /// Publishes scheduler state for a fresh owner-inbox head.
    ///
    /// Like Linux `llist_add()`, the empty-to-nonempty inbox transition itself
    /// owns a physical notification attempt. It must therefore return a
    /// publication even when the sticky owner-work bit was already set: that
    /// older bit may belong to an IPI edge the target has already claimed.
    pub(super) fn publish_owner_inbox_head_owned(&self) -> SchedulerRequestPublication {
        let previous = self
            .scheduler_request
            .request
            .fetch_or(REQUEST_OWNER_WORK, Ordering::AcqRel);
        Self::scheduler_request_publication(previous)
    }

    fn publish_scheduler_request_owned(&self, reason: u64) -> Option<SchedulerRequestPublication> {
        debug_assert_ne!(reason & REQUEST_REASON_MASK, 0);
        let mut observed = self.scheduler_request.request.load(Ordering::Acquire);
        loop {
            // Linux `__resched_curr()` drops a lazy request outright while an
            // ordinary need-resched is live: consuming the ordinary bit also
            // covers the lazy reason, and no lazy residue may survive it.
            let publish = if reason & REQUEST_PREEMPT_LAZY != 0
                && reason & REQUEST_PREEMPT == 0
                && observed & REQUEST_PREEMPT != 0
            {
                reason & !REQUEST_PREEMPT_LAZY
            } else {
                reason
            };
            if publish & REQUEST_REASON_MASK == 0 || observed & publish == publish {
                return None;
            }
            match self.scheduler_request.request.compare_exchange_weak(
                observed,
                observed | publish,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(previous) => return Some(Self::scheduler_request_publication(previous)),
                Err(updated) => observed = updated,
            }
        }
    }

    fn scheduler_request_publication(previous: u64) -> SchedulerRequestPublication {
        let delivery = if previous & REQUEST_IDLE_POLLING != 0 {
            SchedulerRequestDelivery::PollingOwner
        } else {
            SchedulerRequestDelivery::DoorbellRequired
        };
        SchedulerRequestPublication {
            delivery,
            #[cfg(feature = "task-test-hooks")]
            previous_owner_work: previous & REQUEST_OWNER_WORK != 0,
        }
    }

    pub(crate) fn kick_scheduler_work(&self) -> bool {
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_fair_idle_pull_failure_scheduler_kick(self.owner);
        let Some(_publication) = self.begin_owner_delivery() else {
            return false;
        };
        let _irq = IrqScope::enter();
        self.kick_scheduler_work_owned()
    }

    pub(super) fn kick_scheduler_work_owned(&self) -> bool {
        self.request_scheduler_work_owned()
            .is_none_or(|publication| self.deliver_scheduler_work_owned(publication))
    }

    /// Rearms the physical doorbell after an owner-side bounded drain.
    ///
    /// Unlike producer delivery, this must not suppress a local notification:
    /// the current scheduler safe point has already consumed its delivery
    /// edge and is about to return. A remaining batch therefore needs a fresh
    /// interrupt even when the owner itself is the current CPU.
    pub(crate) fn defer_scheduler_work(&self) {
        let Some(_publication) = self.begin_owner_delivery() else {
            task_runtime::fatal_invariant(
                DEFERRED_SCHEDULER_WORK_OFFLINE_INVARIANT,
                self.owner.as_u32() as usize,
            );
        };
        let _irq = IrqScope::enter();
        self.scheduler_request
            .request
            .fetch_or(REQUEST_OWNER_WORK, Ordering::Release);
        self.ring_scheduler_doorbell();
    }

    pub(super) fn deliver_scheduler_work_owned(
        &self,
        publication: SchedulerRequestPublication,
    ) -> bool {
        if publication.delivery == SchedulerRequestDelivery::PollingOwner
            || self.current_cpu_will_service_local_work()
        {
            return true;
        }
        self.ring_scheduler_doorbell()
    }

    fn ring_scheduler_doorbell(&self) -> bool {
        match task_runtime::notify_scheduler_cpu(RuntimeCpuId::new(self.owner.as_u32())) {
            RuntimeStatus::Success => true,
            status => task_runtime::fatal_invariant(
                0x4950_4900 | status as u32,
                self.owner.as_u32() as usize,
            ),
        }
    }

    fn current_cpu_will_service_local_work(&self) -> bool {
        // Every caller retains an IrqScope from before this observation through
        // publication completion, so the runtime CPU identity cannot migrate.
        let current = unsafe { task_runtime::current_cpu_id() };
        if current.as_u32() != self.owner.as_u32() {
            return false;
        }
        // Publish into the architecture preemption word before suppressing a
        // self-IPI. Hard IRQ return consumes that state through its outer
        // preemption guard. Ordinary task publication instead converts the
        // final IRQ guard directly into the scheduler baton.
        task_runtime::publish_local_scheduler_work()
    }

    /// Tests the sticky reschedule request without consuming it.
    pub fn needs_reschedule(&self) -> bool {
        self.scheduler_request.request.load(Ordering::Acquire) & REQUEST_REASON_MASK != 0
    }

    /// Returns whether a kernel preempt-enable or IRQ-return boundary must
    /// enter the scheduler. Lazy Fair preemption alone is intentionally
    /// excluded, matching Linux's folded architecture need-resched word.
    pub(crate) fn needs_immediate_scheduler_work(&self) -> bool {
        self.scheduler_request.request.load(Ordering::Acquire)
            & (REQUEST_PREEMPT | REQUEST_OWNER_WORK)
            != 0
    }

    pub(crate) fn scheduler_request_pending(&self, scope: SchedulerRequestScope) -> bool {
        self.scheduler_request.request.load(Ordering::Acquire) & scope.claim_mask() != 0
    }

    /// Returns whether a sticky preemption request owns scheduler progress.
    ///
    /// Unlike [`Self::needs_reschedule`], owner-only deferred work does not
    /// transfer ownership of the current task's runtime clockevent.
    pub(crate) fn immediate_preemption_requested(&self) -> bool {
        self.scheduler_request.request.load(Ordering::Acquire) & REQUEST_PREEMPT != 0
    }

    pub(crate) fn claim_scheduler_request(
        &self,
        scope: SchedulerRequestScope,
    ) -> SchedulerRequestClaim {
        let request = self
            .scheduler_request
            .request
            .fetch_and(!scope.claim_mask(), Ordering::AcqRel);
        SchedulerRequestClaim {
            immediate_preempt: request & REQUEST_PREEMPT != 0,
            lazy_preempt: request & REQUEST_PREEMPT_LAZY != 0
                && scope == SchedulerRequestScope::All,
        }
    }

    /// Promotes Linux's lazy thread flag at the periodic scheduler tick.
    ///
    /// The caller is the target CPU's timer owner, so no physical delivery is
    /// needed; IRQ return observes the newly published ordinary request.
    pub(crate) fn promote_lazy_reschedule(&self) -> bool {
        let request = self.scheduler_request.request.load(Ordering::Acquire);
        if request & REQUEST_PREEMPT_LAZY == 0 {
            return false;
        }
        self.scheduler_request
            .request
            .fetch_or(REQUEST_PREEMPT, Ordering::AcqRel);
        true
    }

    pub(crate) fn finish_scheduler_request(&self) {
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::publish_preempt_before_bounded_owner_control_rearm(self.owner);
        let request = self.scheduler_request.request.load(Ordering::Acquire);
        if self.has_remote_work() && request & REQUEST_OWNER_WORK == 0 {
            self.request_scheduler_work();
        }
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_bounded_owner_control_ack(
            self.owner,
            self.owner_work_requested_for_test(),
        );
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn take_preempt_requested(&self) -> bool {
        let claim = self.claim_scheduler_request(SchedulerRequestScope::All);
        self.finish_scheduler_request();
        claim.preemption_requested()
    }

    #[cfg(any(test, feature = "task-test-hooks", all(axtest, feature = "axtest")))]
    pub(crate) fn owner_work_requested_for_test(&self) -> bool {
        self.scheduler_request.request.load(Ordering::Acquire) & REQUEST_OWNER_WORK != 0
    }

    pub(crate) fn defer_park_preemption(&self, request: SchedulerRequestClaim) {
        let mut deferred = 0;
        if request.immediate_preempt_requested() {
            deferred |= REQUEST_PARK_PREEMPT_DEFERRED;
        }
        if request.lazy_preempt_requested() {
            deferred |= REQUEST_PARK_PREEMPT_LAZY_DEFERRED;
        }
        if deferred != 0 {
            self.scheduler_request
                .request
                .fetch_or(deferred, Ordering::Release);
        }
    }

    pub(crate) fn finish_park_preemption(&self, resume_running: bool) {
        let deferred = self.scheduler_request.request.fetch_and(
            !(REQUEST_PARK_PREEMPT_DEFERRED | REQUEST_PARK_PREEMPT_LAZY_DEFERRED),
            Ordering::AcqRel,
        );
        if !resume_running {
            return;
        }
        if deferred & REQUEST_PARK_PREEMPT_DEFERRED != 0 {
            let _ = self.request_reschedule_owned(RescheduleKind::Immediate);
        }
        if deferred & REQUEST_PARK_PREEMPT_LAZY_DEFERRED != 0 {
            let _ = self.request_reschedule_owned(RescheduleKind::Lazy);
        }
    }

    pub(crate) fn prepare_idle_wait(&self) -> bool {
        let previous = self
            .scheduler_request
            .request
            .fetch_or(REQUEST_IDLE_POLLING, Ordering::AcqRel);
        let may_wait = previous & REQUEST_REASON_MASK == 0
            && !self.needs_reschedule()
            && !self.has_remote_work()
            && self.queued_summary() == 0;
        if !may_wait {
            self.finish_idle_wait();
        }
        may_wait
    }

    pub(crate) fn finish_idle_wait(&self) {
        self.scheduler_request
            .request
            .fetch_and(!REQUEST_IDLE_POLLING, Ordering::Release);
        // Linux `current_clr_polling()` pairs this full barrier with
        // `resched_curr()`: work published before the clear remains visible
        // to the final IRQ-off recheck, while a producer observing the clear
        // must ring the physical doorbell.
        fence(Ordering::SeqCst);
    }

    /// Observes whether a lazy request survives behind a live ordinary one.
    ///
    /// Linux `__resched_curr()` returns early when `TIF_NEED_RESCHED` is
    /// already set, so a lazy request published in that window must leave no
    /// residue for later scheduling points.
    #[cfg(all(axtest, feature = "axtest"))]
    pub(crate) fn exercise_lazy_request_behind_immediate_for_test() -> (bool, bool) {
        let remote = super::CpuRemote::create(CpuId::new(0), crate::TaskSystemConfig::new(1));
        let _ = remote.request_reschedule_owned(RescheduleKind::Immediate);
        let _ = remote.request_reschedule_owned(RescheduleKind::Lazy);
        let immediate = remote.claim_scheduler_request(SchedulerRequestScope::Immediate);
        let remainder = remote.claim_scheduler_request(SchedulerRequestScope::All);
        (
            immediate.immediate_preempt_requested(),
            remainder.lazy_preempt_requested(),
        )
    }

    pub(crate) fn is_idle_polling(&self) -> bool {
        self.scheduler_request.request.load(Ordering::Acquire) & REQUEST_IDLE_POLLING != 0
    }

    pub(super) fn reset_scheduler_for_offline(&self) {
        self.scheduler_request.request.store(0, Ordering::Relaxed);
    }
}
