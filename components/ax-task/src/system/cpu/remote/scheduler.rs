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
}

impl SchedulerRequestPublication {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SchedulerRequestClaim {
    immediate_preempt: bool,
    lazy_preempt: bool,
    owner_work: bool,
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

    pub(crate) const fn owner_work_requested(self) -> bool {
        self.owner_work
    }

    pub(crate) const fn merge(self, other: Self) -> Self {
        Self {
            immediate_preempt: self.immediate_preempt || other.immediate_preempt,
            lazy_preempt: self.lazy_preempt || other.lazy_preempt,
            owner_work: self.owner_work || other.owner_work,
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

    fn publish(&self, reason: u64) -> Option<u64> {
        debug_assert_ne!(reason & REQUEST_REASON_MASK, 0);
        let mut observed = self.request.load(Ordering::Acquire);
        loop {
            // Linux `__resched_curr()` drops a lazy request while an ordinary
            // need-resched is live: the ordinary scheduling pass covers the
            // lazy reason, so no lazy residue may survive behind it. Retrying
            // the CAS is essential: if the ordinary request was consumed in
            // the meantime, the new lazy reason must be published instead.
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
            match self.request.compare_exchange_weak(
                observed,
                observed | publish,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(previous) => return Some(previous),
                Err(updated) => observed = updated,
            }
        }
    }

    fn publish_remote(&self, reason: u64) -> Option<u64> {
        let published = self.publish(reason);
        if reason & (REQUEST_PREEMPT | REQUEST_OWNER_WORK) == 0 {
            return published;
        }

        // Linux may suppress a repeated reschedule IPI because need-resched
        // belongs to the exact rq current task until that task reaches a safe
        // point. This state is CPU-global, so the sticky bit can outlive the
        // physical edge which first transported it. Preserve the logical bit
        // coalescing while still offering each fresh remote scheduling
        // decision to DeliveryEdge; an armed edge coalesces it, while a
        // claimed edge sends again.
        published.or_else(|| Some(self.request.load(Ordering::Acquire)))
    }

    fn publish_rq_delivery(&self, reason: u64) -> Option<u64> {
        // Unlike Linux's task-owned TIF_NEED_RESCHED, this CPU-global bit can
        // survive the physical edge that first carried it. Each fresh rq
        // decision must therefore retry delivery for immediate or owner work.
        self.publish_remote(reason)
    }

    fn claim(&self, scope: SchedulerRequestScope) -> SchedulerRequestClaim {
        let request = self
            .request
            .fetch_and(!scope.claim_mask(), Ordering::AcqRel);
        SchedulerRequestClaim {
            immediate_preempt: request & REQUEST_PREEMPT != 0,
            lazy_preempt: request & REQUEST_PREEMPT_LAZY != 0
                && scope == SchedulerRequestScope::All,
            owner_work: request & REQUEST_OWNER_WORK != 0,
        }
    }

    fn promote_lazy(&self) -> bool {
        let mut observed = self.request.load(Ordering::Acquire);
        loop {
            if observed & REQUEST_PREEMPT_LAZY == 0 {
                return false;
            }
            // The tick turns the observed lazy reason into an ordinary one.
            // Keeping both bits would let the ordinary scheduling pass clear
            // only its own class and later consume the same reason again.
            let promoted = (observed | REQUEST_PREEMPT) & !REQUEST_PREEMPT_LAZY;
            match self.request.compare_exchange_weak(
                observed,
                promoted,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(updated) => observed = updated,
            }
        }
    }

    fn defer_park_preemption(&self, request: SchedulerRequestClaim) {
        let mut deferred = 0;
        if request.immediate_preempt_requested() {
            deferred |= REQUEST_PARK_PREEMPT_DEFERRED;
        }
        if request.lazy_preempt_requested() {
            deferred |= REQUEST_PARK_PREEMPT_LAZY_DEFERRED;
        }
        if deferred != 0 {
            self.request.fetch_or(deferred, Ordering::Release);
        }
    }

    fn finish_park_preemption(&self, resume_running: bool) {
        let deferred = self.request.fetch_and(
            !(REQUEST_PARK_PREEMPT_DEFERRED | REQUEST_PARK_PREEMPT_LAZY_DEFERRED),
            Ordering::AcqRel,
        );
        if !resume_running {
            return;
        }
        if deferred & REQUEST_PARK_PREEMPT_DEFERRED != 0 {
            let _ = self.publish(REQUEST_PREEMPT);
        }
        if deferred & REQUEST_PARK_PREEMPT_LAZY_DEFERRED != 0 {
            let _ = self.publish(REQUEST_PREEMPT_LAZY);
        }
    }

    fn restore_claimed_park_preemption(&self, request: SchedulerRequestClaim) {
        self.defer_park_preemption(request);
        self.finish_park_preemption(true);
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

    fn publish_scheduler_reasons_owned(
        &self,
        reschedule: Option<RescheduleKind>,
        owner_work: bool,
    ) {
        let mut reasons = reschedule.map_or(0, RescheduleKind::request_bit);
        if owner_work {
            reasons |= REQUEST_OWNER_WORK;
        }
        let publication = self
            .scheduler_request
            .publish_remote(reasons)
            .map(Self::scheduler_request_publication);
        if owner_work || reschedule == Some(RescheduleKind::Immediate) {
            self.deliver_scheduler_work_owned(
                publication.expect("immediate remote scheduler work must retain a publication"),
            );
        }
    }

    /// Publishes scheduler reasons selected while the target runqueue is locked.
    ///
    /// This is the direct equivalent of Linux `resched_curr(rq)`: the producer
    /// CPU is already pinned, target placement is serialized by `p->pi_lock`,
    /// and the target rq remains locked until the caller commits the enqueue.
    /// Local work therefore updates the architecture preemption word directly;
    /// only a remote target needs the scheduler doorbell.
    pub(crate) fn publish_rq_scheduler_reasons(
        &self,
        reschedule: Option<RescheduleKind>,
        owner_work: bool,
        producer: CpuId,
        irq_owner: &IrqOwner<'_>,
    ) {
        if reschedule.is_none() && !owner_work {
            return;
        }
        // The caller already owns the task-scheduler IRQ-save guard and keeps
        // it live while the target rq transaction commits. Linux publishes
        // `TIF_NEED_RESCHED` from that same `p->pi_lock`/rq critical section;
        // opening another runtime IRQ guard here only increments the nested
        // depth and rechecks the same CPU owner.
        let _ = irq_owner;
        let mut reasons = reschedule.map_or(0, RescheduleKind::request_bit);
        if owner_work {
            reasons |= REQUEST_OWNER_WORK;
        }
        if let Some(publication) = self
            .scheduler_request
            .publish_rq_delivery(reasons)
            .map(Self::scheduler_request_publication)
            && (owner_work || reschedule == Some(RescheduleKind::Immediate))
        {
            if publication.delivery == SchedulerRequestDelivery::PollingOwner {
                return;
            }
            if producer == self.owner {
                let _self_serviced = task_runtime::publish_local_scheduler_work();
            } else {
                self.ring_scheduler_doorbell();
            }
        }
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
        self.publish_scheduler_reasons_owned(Some(kind), false);
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
        self.publish_scheduler_reasons_owned(Some(kind), true);
    }

    pub(crate) fn request_scheduler_work(&self) {
        let _delivered = self.request_scheduler_work_delivery();
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
        self.scheduler_request
            .publish(reason)
            .map(Self::scheduler_request_publication)
    }

    fn scheduler_request_publication(previous: u64) -> SchedulerRequestPublication {
        let delivery = if previous & REQUEST_IDLE_POLLING != 0 {
            SchedulerRequestDelivery::PollingOwner
        } else {
            SchedulerRequestDelivery::DoorbellRequired
        };
        SchedulerRequestPublication { delivery }
    }

    pub(crate) fn kick_scheduler_work(&self) -> bool {
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
        self.scheduler_request.claim(scope)
    }

    /// Promotes Linux's lazy thread flag at the periodic scheduler tick.
    ///
    /// The caller is the target CPU's timer owner, so no physical delivery is
    /// needed; IRQ return observes the newly published ordinary request.
    pub(crate) fn promote_lazy_reschedule(&self) -> bool {
        self.scheduler_request.promote_lazy()
    }

    pub(crate) fn finish_scheduler_request(&self) {
        let request = self.scheduler_request.request.load(Ordering::Acquire);
        if self.has_remote_work() && request & REQUEST_OWNER_WORK == 0 {
            self.request_scheduler_work();
        }
    }

    pub(crate) fn defer_park_preemption(&self, request: SchedulerRequestClaim) {
        self.scheduler_request.defer_park_preemption(request);
    }

    pub(crate) fn finish_park_preemption(&self, resume_running: bool) {
        self.scheduler_request
            .finish_park_preemption(resume_running);
    }

    /// Restores a scheduler request claimed by a park that was cancelled
    /// before it could publish Blocked.
    pub(crate) fn restore_claimed_park_preemption(&self, request: SchedulerRequestClaim) {
        self.scheduler_request
            .restore_claimed_park_preemption(request);
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

    pub(crate) fn is_idle_polling(&self) -> bool {
        self.scheduler_request.request.load(Ordering::Acquire) & REQUEST_IDLE_POLLING != 0
    }

    pub(super) fn reset_scheduler_for_offline(&self) {
        self.scheduler_request.request.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_request_folds_a_later_lazy_request() {
        let request = SchedulerRequestState::new();
        assert!(request.publish(REQUEST_PREEMPT).is_some());
        assert!(
            request.publish(REQUEST_PREEMPT_LAZY).is_none(),
            "a live ordinary request must cover the lazy reason"
        );

        let immediate = request.request.fetch_and(
            !SchedulerRequestScope::Immediate.claim_mask(),
            Ordering::AcqRel,
        );
        assert_ne!(immediate & REQUEST_PREEMPT, 0);
        assert_eq!(
            request.request.load(Ordering::Acquire) & REQUEST_PREEMPT_LAZY,
            0,
            "the ordinary scheduling pass must not leave lazy residue"
        );

        assert!(
            request.publish(REQUEST_PREEMPT_LAZY).is_some(),
            "a lazy reason published after ordinary consumption is new work"
        );
        assert_ne!(
            request.request.load(Ordering::Acquire) & REQUEST_PREEMPT_LAZY,
            0
        );
    }

    #[test]
    fn periodic_tick_moves_lazy_request_into_ordinary_class() {
        let request = SchedulerRequestState::new();
        assert!(request.publish(REQUEST_PREEMPT_LAZY).is_some());
        assert!(request.promote_lazy());

        let claim = request.claim(SchedulerRequestScope::Immediate);
        assert!(claim.immediate_preempt_requested());
        assert!(
            !request
                .claim(SchedulerRequestScope::All)
                .preemption_requested(),
            "the ordinary scheduling pass must consume the lazy reason promoted by this tick"
        );
    }

    #[test]
    fn notified_park_restores_its_claimed_preemption_request() {
        let ordinary = SchedulerRequestState::new();
        let _ = ordinary.publish(REQUEST_PREEMPT);
        let claim = ordinary.claim(SchedulerRequestScope::All);
        ordinary.restore_claimed_park_preemption(claim);

        assert_ne!(
            ordinary.request.load(Ordering::Acquire) & REQUEST_PREEMPT,
            0,
            "a notified park must restore the ordinary request claimed before cancellation"
        );

        let lazy = SchedulerRequestState::new();
        let _ = lazy.publish(REQUEST_PREEMPT_LAZY);
        let claim = lazy.claim(SchedulerRequestScope::All);
        lazy.restore_claimed_park_preemption(claim);
        assert_ne!(
            lazy.request.load(Ordering::Acquire) & REQUEST_PREEMPT_LAZY,
            0,
            "a notified park must restore the lazy request claimed before cancellation"
        );
    }

    #[test]
    fn repeated_remote_preemption_retains_a_delivery_attempt() {
        let request = SchedulerRequestState::new();

        assert!(request.publish_remote(REQUEST_PREEMPT).is_some());
        assert!(
            request.publish_remote(REQUEST_PREEMPT).is_some(),
            "a fresh remote rq decision must reach DeliveryEdge even while the CPU bit is sticky"
        );
        assert!(
            request.publish_remote(REQUEST_PREEMPT_LAZY).is_none(),
            "a repeated lazy request must remain logical-only"
        );
    }
}
