use super::*;

const SCHEDULER_WORK_PENDING: u8 = 1 << 0;
const SCHEDULER_IDLE_POLLING: u8 = 1 << 1;
const DEFERRED_SCHEDULER_WORK_OFFLINE_INVARIANT: u32 = 0x4453_574f;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SchedulerWorkPublication {
    PollingOwner,
    DoorbellRequired,
}

#[derive(Debug)]
pub(super) struct SchedulerDoorbellState {
    ready: AtomicBool,
    flags: AtomicU8,
    deadline_work_pending: AtomicBool,
    preempt_requested: AtomicBool,
    park_preempt_deferred: AtomicBool,
}

impl SchedulerDoorbellState {
    pub(super) const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            flags: AtomicU8::new(0),
            deadline_work_pending: AtomicBool::new(false),
            preempt_requested: AtomicBool::new(false),
            park_preempt_deferred: AtomicBool::new(false),
        }
    }
}

impl CpuRemote {
    pub(crate) fn mark_scheduler_ready(&self) {
        self.scheduler.ready.store(true, Ordering::Release);
    }

    pub(crate) fn is_scheduler_ready(&self) -> bool {
        self.scheduler.ready.load(Ordering::Acquire)
    }

    /// Publishes a sticky owner-CPU reschedule request.
    pub(crate) fn request_reschedule(&self) {
        let Some(_publication) = self.begin_publication() else {
            return;
        };
        self.request_reschedule_owned();
    }

    fn request_reschedule_owned(&self) -> SchedulerWorkPublication {
        self.scheduler
            .preempt_requested
            .store(true, Ordering::Release);
        self.request_scheduler_work_owned()
    }

    /// Publishes a remote preemption and rings the target doorbell only after
    /// the runqueue transaction has become visible.
    pub(crate) fn request_remote_reschedule(&self) {
        let Some(_publication) = self.begin_owner_delivery() else {
            return;
        };
        let _irq = IrqScope::enter();
        let publication = self.request_reschedule_owned();
        self.deliver_scheduler_work_owned(publication);
    }

    pub(crate) fn request_scheduler_work(&self) {
        let Some(_publication) = self.begin_owner_delivery() else {
            return;
        };
        self.request_scheduler_work_owned();
    }

    pub(super) fn request_scheduler_work_owned(&self) -> SchedulerWorkPublication {
        let previous = self
            .scheduler
            .flags
            .fetch_or(SCHEDULER_WORK_PENDING, Ordering::AcqRel);
        if previous & SCHEDULER_IDLE_POLLING != 0 {
            SchedulerWorkPublication::PollingOwner
        } else {
            SchedulerWorkPublication::DoorbellRequired
        }
    }

    pub(in crate::system::cpu) fn publish_deadline_work(&self) {
        self.scheduler
            .deadline_work_pending
            .store(true, Ordering::Release);
        let _ = self.request_scheduler_work_owned();
    }

    pub(crate) fn deadline_work_pending(&self) -> bool {
        self.scheduler.deadline_work_pending.load(Ordering::Acquire)
    }

    pub(in crate::system::cpu) fn begin_deadline_work(&self) -> bool {
        self.scheduler
            .deadline_work_pending
            .swap(false, Ordering::AcqRel)
    }

    pub(in crate::system::cpu) fn finish_deadline_work(&self, pending: bool) {
        // Only the owner CPU publishes deadline work, and both timer IRQ and
        // scheduler safe-point paths hold local IRQ exclusion while mutating
        // CpuLocal. The completed pass therefore owns the full publication
        // interval and may replace the sticky bit with its actual remainder.
        self.scheduler
            .deadline_work_pending
            .store(pending, Ordering::Release);
        if pending {
            let _ = self.request_scheduler_work_owned();
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
        self.request_scheduler_work_owned();
        self.ring_scheduler_doorbell();
    }

    pub(super) fn deliver_scheduler_work_owned(
        &self,
        publication: SchedulerWorkPublication,
    ) -> bool {
        if publication == SchedulerWorkPublication::PollingOwner
            || self.current_cpu_will_service_local_work()
        {
            return true;
        }
        self.ring_scheduler_doorbell()
    }

    fn ring_scheduler_doorbell(&self) -> bool {
        match task_runtime::send_scheduler_ipi(RuntimeCpuId::new(self.owner.as_u32())) {
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
        self.scheduler.flags.load(Ordering::Acquire) & SCHEDULER_WORK_PENDING != 0
    }

    pub(crate) fn scheduler_enter(&self) -> bool {
        self.scheduler
            .flags
            .fetch_and(!SCHEDULER_WORK_PENDING, Ordering::AcqRel);
        let preempt_requested = self
            .scheduler
            .preempt_requested
            .swap(false, Ordering::AcqRel);
        if self.deadline_work_pending() || self.has_remote_work() {
            self.request_scheduler_work();
        }
        preempt_requested
    }

    #[cfg(test)]
    pub(crate) fn take_preempt_requested(&self) -> bool {
        self.scheduler
            .preempt_requested
            .swap(false, Ordering::AcqRel)
    }

    pub(crate) fn defer_park_preemption(&self, requested: bool) {
        if requested {
            self.scheduler
                .park_preempt_deferred
                .store(true, Ordering::Release);
        }
    }

    pub(crate) fn finish_park_preemption(&self, resume_running: bool) {
        let deferred = self
            .scheduler
            .park_preempt_deferred
            .swap(false, Ordering::AcqRel);
        if resume_running && deferred {
            let _ = self.request_reschedule_owned();
        }
    }

    pub(crate) fn prepare_idle_wait(&self) -> bool {
        let previous = self
            .scheduler
            .flags
            .fetch_or(SCHEDULER_IDLE_POLLING, Ordering::AcqRel);
        let may_wait = previous & SCHEDULER_WORK_PENDING == 0
            && !self.needs_reschedule()
            && !self.deadline_work_pending()
            && !self.has_remote_work()
            && self.try_runnable_summary() == Some(0);
        if !may_wait {
            self.finish_idle_wait();
        }
        may_wait
    }

    pub(crate) fn finish_idle_wait(&self) {
        self.scheduler
            .flags
            .fetch_and(!SCHEDULER_IDLE_POLLING, Ordering::Release);
    }

    pub(crate) fn is_idle_polling(&self) -> bool {
        self.scheduler.flags.load(Ordering::Acquire) & SCHEDULER_IDLE_POLLING != 0
    }

    pub(super) fn reset_scheduler_for_offline(&self) {
        self.scheduler.flags.store(0, Ordering::Relaxed);
        self.scheduler
            .deadline_work_pending
            .store(false, Ordering::Relaxed);
        self.scheduler
            .preempt_requested
            .store(false, Ordering::Relaxed);
        self.scheduler
            .park_preempt_deferred
            .store(false, Ordering::Relaxed);
    }
}
