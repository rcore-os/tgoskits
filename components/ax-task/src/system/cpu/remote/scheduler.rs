use core::sync::atomic::fence;

use super::*;

const REQUEST_PREEMPT: u64 = 1 << 0;
const REQUEST_OWNER_WORK: u64 = 1 << 1;
const REQUEST_HARD_TIMER: u64 = 1 << 5;
const REQUEST_REASON_MASK: u64 = REQUEST_PREEMPT | REQUEST_OWNER_WORK | REQUEST_HARD_TIMER;
const REQUEST_ENTRY_MASK: u64 = REQUEST_PREEMPT | REQUEST_OWNER_WORK;
const REQUEST_IDLE_POLLING: u64 = 1 << 3;
const REQUEST_PARK_PREEMPT_DEFERRED: u64 = 1 << 4;
const REQUEST_GENERATION_SHIFT: u32 = 8;
const REQUEST_FLAGS_MASK: u64 = (1 << REQUEST_GENERATION_SHIFT) - 1;
const REQUEST_GENERATION_MAX: u64 = u64::MAX >> REQUEST_GENERATION_SHIFT;
const DEFERRED_SCHEDULER_WORK_OFFLINE_INVARIANT: u32 = 0x4453_574f;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SchedulerRequestDelivery {
    /// The owner is in the IRQ-disabled idle polling region and will observe
    /// the sticky work bit before committing to sleep.
    PollingOwner,
    /// The runtime must notify the shared physical IPI delivery edge.
    ///
    /// The logical request generation remains entirely in ax-task. The
    /// runtime transports only a coalescible edge and cannot acknowledge or
    /// replace that logical state.
    DoorbellRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SchedulerRequestPublication {
    delivery: SchedulerRequestDelivery,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SchedulerRequestClaim {
    generation: u64,
    preempt: bool,
}

impl SchedulerRequestClaim {
    pub(crate) const fn preempt_requested(self) -> bool {
        self.preempt
    }

    pub(crate) const fn merge(self, other: Self) -> Self {
        Self {
            generation: if self.generation > other.generation {
                self.generation
            } else {
                other.generation
            },
            preempt: self.preempt || other.preempt,
        }
    }
}

#[derive(Debug)]
pub(super) struct SchedulerRequestState {
    request: AtomicU64,
    acknowledged_generation: AtomicU64,
}

impl SchedulerRequestState {
    pub(super) const fn new() -> Self {
        Self {
            request: AtomicU64::new(0),
            acknowledged_generation: AtomicU64::new(0),
        }
    }
}

const fn request_generation(word: u64) -> u64 {
    word >> REQUEST_GENERATION_SHIFT
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
        let publication = self.scheduler_request.request.try_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |word| {
                if word & REQUEST_PREEMPT != 0 {
                    return None;
                }
                let generation = request_generation(word).checked_add(1)?;
                if generation > REQUEST_GENERATION_MAX {
                    return None;
                }
                Some(
                    (generation << REQUEST_GENERATION_SHIFT)
                        | (word & REQUEST_FLAGS_MASK)
                        | REQUEST_PREEMPT,
                )
            },
        );
        match publication {
            Ok(previous) => Some(Self::scheduler_request_publication(previous)),
            Err(current) if current & REQUEST_PREEMPT != 0 => None,
            Err(_) => panic!("scheduler request generation exhausted"),
        }
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
    /// remain separate sticky bits in one logical generation, matching Linux's
    /// rule that scheduler state and deferred work are visible before the IPI.
    pub(crate) fn request_remote_reschedule_with_scheduler_work(&self) {
        let Some(_publication) = self.begin_owner_delivery() else {
            return;
        };
        let _irq = IrqScope::enter();
        let publication =
            self.publish_scheduler_request_owned(REQUEST_PREEMPT | REQUEST_OWNER_WORK);
        self.deliver_scheduler_work_owned(publication);
    }

    pub(crate) fn request_scheduler_work(&self) {
        let _delivered = self.request_scheduler_work_delivery();
    }

    #[cfg(feature = "task-test-hooks")]
    pub(crate) fn request_scheduler_work_for_test(&self) -> bool {
        self.request_scheduler_work_delivery()
    }

    fn request_scheduler_work_delivery(&self) -> bool {
        let Some(_publication) = self.begin_owner_delivery() else {
            return false;
        };
        let _irq = IrqScope::enter();
        let publication = self.request_scheduler_work_owned();
        self.deliver_scheduler_work_owned(publication)
    }

    pub(super) fn request_scheduler_work_owned(&self) -> SchedulerRequestPublication {
        self.publish_scheduler_request_owned(REQUEST_OWNER_WORK)
    }

    fn publish_scheduler_request_owned(&self, reason: u64) -> SchedulerRequestPublication {
        debug_assert_ne!(reason & REQUEST_REASON_MASK, 0);
        let previous = self
            .scheduler_request
            .request
            .try_update(Ordering::AcqRel, Ordering::Acquire, |word| {
                let generation = request_generation(word).checked_add(1)?;
                if generation > REQUEST_GENERATION_MAX {
                    return None;
                }
                Some(
                    (generation << REQUEST_GENERATION_SHIFT) | (word & REQUEST_FLAGS_MASK) | reason,
                )
            })
            .unwrap_or_else(|_| panic!("scheduler request generation exhausted"));
        Self::scheduler_request_publication(previous)
    }

    fn scheduler_request_publication(previous: u64) -> SchedulerRequestPublication {
        let delivery = if previous & REQUEST_IDLE_POLLING != 0 {
            SchedulerRequestDelivery::PollingOwner
        } else {
            SchedulerRequestDelivery::DoorbellRequired
        };
        SchedulerRequestPublication { delivery }
    }

    pub(in crate::system::cpu) fn publish_hard_timer_work(&self) {
        let _ = self.publish_scheduler_request_owned(REQUEST_HARD_TIMER);
    }

    pub(in crate::system::cpu) fn begin_hard_timer_work(&self) -> bool {
        self.scheduler_request
            .request
            .fetch_and(!REQUEST_HARD_TIMER, Ordering::AcqRel)
            & REQUEST_HARD_TIMER
            != 0
    }

    pub(in crate::system::cpu) fn finish_hard_timer_work(&self, pending: bool) {
        if pending {
            let _ = self.publish_scheduler_request_owned(REQUEST_HARD_TIMER);
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
        let publication = self.request_scheduler_work_owned();
        self.deliver_scheduler_work_owned(publication)
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
        let _publication = self.request_scheduler_work_owned();
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
        let request = self.scheduler_request.request.load(Ordering::Acquire);
        request & REQUEST_REASON_MASK != 0
            || request_generation(request)
                != self
                    .scheduler_request
                    .acknowledged_generation
                    .load(Ordering::Acquire)
    }

    pub(crate) fn claim_scheduler_request(&self) -> SchedulerRequestClaim {
        let request = self
            .scheduler_request
            .request
            .fetch_and(!REQUEST_ENTRY_MASK, Ordering::AcqRel);
        SchedulerRequestClaim {
            generation: request_generation(request),
            preempt: request & REQUEST_PREEMPT != 0,
        }
    }

    pub(crate) fn acknowledge_scheduler_request(&self, claim: SchedulerRequestClaim) {
        self.scheduler_request
            .acknowledged_generation
            .store(claim.generation, Ordering::Release);
        let request = self.scheduler_request.request.load(Ordering::Acquire);
        if self.has_remote_work() && request & REQUEST_ENTRY_MASK == 0 {
            self.request_scheduler_work();
        }
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn take_preempt_requested(&self) -> bool {
        let claim = self.claim_scheduler_request();
        self.acknowledge_scheduler_request(claim);
        claim.preempt_requested()
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn scheduler_request_state_for_test(&self) -> (u64, u64, u64) {
        let request = self.scheduler_request.request.load(Ordering::Acquire);
        (
            request_generation(request),
            self.scheduler_request
                .acknowledged_generation
                .load(Ordering::Acquire),
            request & REQUEST_REASON_MASK,
        )
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
        self.scheduler_request
            .acknowledged_generation
            .store(0, Ordering::Relaxed);
    }
}
