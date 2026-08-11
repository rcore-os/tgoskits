//! Remotely observable runqueue and owner-work publication state.

use super::*;

mod deadline;
mod delivery;
mod idle_pull;
mod ktimer;
mod lifecycle;
mod load_summary;
mod owner;
mod run_queue;
mod scheduler;

pub(crate) use deadline::{CpuDeadlineState, SchedulerDeadlinePublicationState};
pub(crate) use delivery::PreparedMigrationDelivery;
pub(crate) use idle_pull::IdlePullReservation;
pub use lifecycle::CpuLifecycleState;
pub(crate) use lifecycle::{CpuRemotePublication, OwnedCpuRemotePublication};
pub use owner::CpuLocalOwnerBorrow;
pub(in crate::system::cpu) use run_queue::RqCurrentTick;
pub(crate) use run_queue::{CpuRunQueueState, WakePreemptionDecision};
pub(crate) use scheduler::SchedulerRequestClaim;

#[cfg(test)]
std::thread_local! {
    static RT_BANDWIDTH_LOCK_ACQUISITIONS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
}

#[cfg(test)]
pub(crate) fn reset_rt_bandwidth_lock_acquisitions() {
    RT_BANDWIDTH_LOCK_ACQUISITIONS.set(0);
}

#[cfg(test)]
pub(crate) fn rt_bandwidth_lock_acquisitions() -> usize {
    RT_BANDWIDTH_LOCK_ACQUISITIONS.get()
}

/// Stable cross-CPU publication endpoint for one scheduler owner.
///
/// This object owns the IRQ-safe target runqueue, atomic delivery state, and
/// intrusive owner-control inboxes. Owner-only runtime accounting and switch
/// tail state remain in [`CpuLocal`].
#[derive(Debug)]
pub struct CpuRemote {
    owner: CpuId,
    run_queue: IrqTicketLock<CpuRunQueueState>,
    rt_bandwidth: IrqTicketLock<RtRunQueueBandwidth>,
    deadline: IrqTicketLock<CpuDeadlineState>,
    /// Linux `dl_rq.extra_bw`: root-domain bandwidth published for this rq.
    deadline_extra_bw_scaled: AtomicU64,
    owner_state: owner::OwnerState,
    publication: lifecycle::CpuPublicationState,
    scheduler_request: scheduler::SchedulerRequestState,
    ktimer: ktimer::KtimerWorkerState,
    load: load_summary::RemoteLoadState,
    idle_pull: idle_pull::IdlePullState,
    delivery: delivery::RemoteDeliveryState,
}

impl CpuRemote {
    pub(crate) fn create(owner: CpuId, config: TaskSystemConfig) -> Arc<Self> {
        let deadline_max_bw_scaled =
            u64::from(config.deadline_cap_percent()) * crate::DEADLINE_UTILIZATION_SCALE / 100;
        Arc::new(Self {
            owner,
            run_queue: IrqTicketLock::new(CpuRunQueueState::new(owner, config)),
            rt_bandwidth: IrqTicketLock::new(RtRunQueueBandwidth::offline()),
            deadline: IrqTicketLock::new(CpuDeadlineState::new(config)),
            deadline_extra_bw_scaled: AtomicU64::new(deadline_max_bw_scaled),
            owner_state: owner::OwnerState::new(),
            publication: lifecycle::CpuPublicationState::new(),
            scheduler_request: scheduler::SchedulerRequestState::new(),
            ktimer: ktimer::KtimerWorkerState::new(),
            load: load_summary::RemoteLoadState::new(),
            idle_pull: idle_pull::IdlePullState::new(),
            delivery: delivery::RemoteDeliveryState::new(),
        })
    }

    /// Acquires the target CPU runqueue with local IRQs disabled.
    ///
    /// Thread scheduler state must be acquired before this lock whenever one
    /// transaction needs both. Owner-only switch-tail state is never protected
    /// by this lock and must not escape its CPU-local scheduler baton.
    pub(crate) fn lock_run_queue(&self) -> IrqTicketGuard<'_, CpuRunQueueState> {
        self.run_queue
            .lock(crate::runtime::IrqGuardSource::CpuRunQueueTicket)
    }

    /// Acquires the rq under an already-active IRQ-off CPU owner.
    ///
    /// # Safety
    ///
    /// The caller must retain either the scheduler baton or the offline boot
    /// CPU's Linux-style `PREEMPT_DISABLED` ownership, with local IRQs disabled
    /// for the complete guard lifetime. See
    /// [`IrqTicketLock::lock_irq_disabled`].
    pub(crate) unsafe fn lock_run_queue_irq_disabled(
        &self,
    ) -> IrqTicketGuard<'_, CpuRunQueueState> {
        // SAFETY: forwarded unchanged to the caller's scheduler-baton contract.
        unsafe { self.run_queue.lock_irq_disabled() }
    }

    /// Locks this CPU's hrtimer-style task-deadline base.
    ///
    /// The rq lock precedes this lock when both are required. Timer IRQ code
    /// takes only this lock; soft-timer callbacks release it before acquiring a
    /// task control lock or rq lock.
    pub(crate) fn lock_deadline_base(&self) -> IrqTicketGuard<'_, CpuDeadlineState> {
        self.deadline
            .lock(crate::runtime::IrqGuardSource::CpuDeadlineTicket)
    }

    /// Locks Linux `rt_rq::rt_runtime_lock` after the owner rq lock when both
    /// are required. Fair-only rq transactions never enter this ledger.
    pub(crate) fn lock_rt_bandwidth(&self) -> IrqTicketGuard<'_, RtRunQueueBandwidth> {
        #[cfg(test)]
        RT_BANDWIDTH_LOCK_ACQUISITIONS.set(RT_BANDWIDTH_LOCK_ACQUISITIONS.get().saturating_add(1));
        self.rt_bandwidth
            .lock(crate::runtime::IrqGuardSource::CpuRtBandwidthTicket)
    }

    pub(crate) fn publish_deadline_extra_bw(&self, extra_bw_scaled: u64) {
        self.deadline_extra_bw_scaled
            .store(extra_bw_scaled, Ordering::Release);
    }

    pub(crate) fn deadline_extra_bw_scaled(&self) -> u64 {
        self.deadline_extra_bw_scaled.load(Ordering::Acquire)
    }
}

include!("tests.rs");
