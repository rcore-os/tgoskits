use core::sync::atomic::fence;

use super::*;

const REQUEST_PREEMPT: u64 = 1 << 0;
const REQUEST_OWNER_WORK: u64 = 1 << 1;
const REQUEST_REASON_MASK: u64 = REQUEST_PREEMPT | REQUEST_OWNER_WORK;
const REQUEST_ENTRY_MASK: u64 = REQUEST_PREEMPT | REQUEST_OWNER_WORK;
const REQUEST_IDLE_POLLING: u64 = 1 << 3;
const REQUEST_PARK_PREEMPT_DEFERRED: u64 = 1 << 4;
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
    preempt: bool,
}

impl SchedulerRequestClaim {
    pub(crate) const fn preempt_requested(self) -> bool {
        self.preempt
    }

    pub(crate) const fn merge(self, other: Self) -> Self {
        Self {
            preempt: self.preempt || other.preempt,
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
    pub(crate) fn request_reschedule(&self) {
        let Some(_publication) = self.begin_publication() else {
            return;
        };
        let _ = self.request_reschedule_owned();
    }

    fn request_reschedule_owned(&self) -> Option<SchedulerRequestPublication> {
        self.publish_scheduler_request_owned(REQUEST_PREEMPT)
    }

    /// Publishes a remote preemption and rings the target doorbell only after
    /// the runqueue transaction has become visible.
    pub(crate) fn request_remote_reschedule(&self) {
        let Some(_publication) = self.begin_owner_delivery() else {
            return;
        };
        let _irq = IrqScope::enter();
        if let Some(publication) = self.request_reschedule_owned() {
            self.deliver_scheduler_work_owned(publication);
        }
    }

    /// Publishes coupled preemption and owner-work reasons before ringing one
    /// physical scheduler doorbell.
    ///
    /// One rq transaction may make both facts true. They share transport but
    /// remain separate sticky bits, matching Linux's rule that scheduler state
    /// and deferred work are visible before the IPI.
    pub(crate) fn request_remote_reschedule_with_scheduler_work(&self) {
        let Some(_publication) = self.begin_owner_delivery() else {
            return;
        };
        let _irq = IrqScope::enter();
        if let Some(publication) =
            self.publish_scheduler_request_owned(REQUEST_PREEMPT | REQUEST_OWNER_WORK)
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
        let Some(publication) =
            self.publish_scheduler_request_owned(REQUEST_PREEMPT | REQUEST_OWNER_WORK)
        else {
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
        let previous = self
            .scheduler_request
            .request
            .fetch_or(reason, Ordering::AcqRel);
        if previous & reason == reason {
            None
        } else {
            Some(Self::scheduler_request_publication(previous))
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

    /// Returns whether a sticky preemption request owns scheduler progress.
    ///
    /// Unlike [`Self::needs_reschedule`], owner-only deferred work does not
    /// transfer ownership of the current task's runtime clockevent.
    pub(crate) fn preemption_requested(&self) -> bool {
        self.scheduler_request.request.load(Ordering::Acquire) & REQUEST_PREEMPT != 0
    }

    pub(crate) fn claim_scheduler_request(&self) -> SchedulerRequestClaim {
        let request = self
            .scheduler_request
            .request
            .fetch_and(!REQUEST_ENTRY_MASK, Ordering::AcqRel);
        SchedulerRequestClaim {
            preempt: request & REQUEST_PREEMPT != 0,
        }
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
        let claim = self.claim_scheduler_request();
        self.finish_scheduler_request();
        claim.preempt_requested()
    }

    #[cfg(any(test, feature = "task-test-hooks", all(axtest, feature = "axtest")))]
    pub(crate) fn owner_work_requested_for_test(&self) -> bool {
        self.scheduler_request.request.load(Ordering::Acquire) & REQUEST_OWNER_WORK != 0
    }

    pub(crate) fn defer_park_preemption(&self, requested: bool) {
        if requested {
            self.scheduler_request
                .request
                .fetch_or(REQUEST_PARK_PREEMPT_DEFERRED, Ordering::Release);
        }
    }

    pub(crate) fn finish_park_preemption(&self, resume_running: bool) {
        let deferred = self
            .scheduler_request
            .request
            .fetch_and(!REQUEST_PARK_PREEMPT_DEFERRED, Ordering::AcqRel)
            & REQUEST_PARK_PREEMPT_DEFERRED
            != 0;
        if resume_running && deferred {
            let _ = self.request_reschedule_owned();
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

    pub(crate) fn is_idle_polling(&self) -> bool {
        self.scheduler_request.request.load(Ordering::Acquire) & REQUEST_IDLE_POLLING != 0
    }

    pub(super) fn reset_scheduler_for_offline(&self) {
        self.scheduler_request.request.store(0, Ordering::Relaxed);
    }
}
