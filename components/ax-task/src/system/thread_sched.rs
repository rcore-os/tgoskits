//! Per-thread scheduler state owned independently from the generation registry.

use alloc::sync::Weak;

use crate::{
    CpuId, CpuSet, DeadlineEntity, SchedulePolicy, SchedulingEntity, TaskError, ThreadCore,
    ThreadId, ThreadLifecycle, ThreadState,
    lock::{IrqTicketGuard, IrqTicketLock},
    runtime::{AddressSpaceHandle, ExecutionContextHandle},
};

/// GRUB activity of one admitted Deadline reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeadlineActivity {
    /// Ready or executing, and therefore contributing active utilization.
    ActiveContending,
    /// Blocked before zero-lag while still contributing active utilization.
    ActiveNonContending,
    /// Blocked past zero-lag and eligible to donate inactive utilization.
    Inactive,
}

/// Stable scheduler ownership anchor retained by every runnable reference.
///
/// Owner CPUs operate on this cell through queued, current, and inbox-held
/// `ThreadCore` references. Registry locking is reserved for lifecycle lookup,
/// admission, and PI graph changes rather than owner runqueue progress.
#[derive(Debug)]
pub(crate) struct ThreadSchedCell {
    id: ThreadId,
    state: IrqTicketLock<ThreadSchedState>,
}

impl ThreadSchedCell {
    pub(super) fn new(id: ThreadId, state: ThreadSchedState) -> Self {
        Self {
            id,
            state: IrqTicketLock::new(state),
        }
    }

    pub(crate) const fn id(&self) -> ThreadId {
        self.id
    }

    pub(super) fn lock(&self) -> IrqTicketGuard<'_, ThreadSchedState> {
        self.state.lock()
    }

    #[cfg(test)]
    pub(crate) fn new_test(id: ThreadId, policy: SchedulePolicy) -> Self {
        let entity = SchedulingEntity::new(policy, 1, 0);
        Self::new(
            id,
            ThreadSchedState {
                lifecycle: ThreadLifecycle::new(),
                base_policy: policy,
                active_base_policy: policy,
                policy,
                policy_generation: 1,
                applied_policy_generation: 1,
                dispatch_generation: 1,
                affinity: CpuSet::all(1),
                entity,
                base_entity: entity,
                base_deadline: entity.deadline(),
                deadline_activity: DeadlineActivity::Inactive,
                deadline_bandwidth_cpu: None,
                deadline_cleanup_pending: false,
                deadline_bandwidth_scaled: 0,
                active_deadline_reservation: 0,
                desired_deadline_reservation: 0,
                deadline_zero_lag_ns: 0,
                queued_cpu: None,
                running_cpu: None,
                on_cpu: None,
                migration_target: None,
                blocked_pi_waiters: 0,
                pi_donor: None,
                deadline_donor: None,
                deadline_donor_core: None,
                deadline_cbs_borrower: None,
                deadline_cbs_generation: 1,
                pi_critical_rescue: false,
                deadline_replenish_pending: false,
                deadline_overrun_events: 0,
                charged_runtime_ns: 0,
                context: ExecutionContextHandle::NONE,
                address_space: AddressSpaceHandle::NONE,
            },
        )
    }
}

#[derive(Debug)]
pub(super) struct ThreadSchedState {
    pub(super) lifecycle: ThreadLifecycle,
    pub(super) base_policy: SchedulePolicy,
    pub(super) active_base_policy: SchedulePolicy,
    pub(super) policy: SchedulePolicy,
    pub(super) policy_generation: u64,
    pub(super) applied_policy_generation: u64,
    pub(super) dispatch_generation: u64,
    pub(super) affinity: CpuSet,
    pub(super) entity: SchedulingEntity,
    pub(super) base_entity: SchedulingEntity,
    pub(super) base_deadline: Option<DeadlineEntity>,
    pub(super) deadline_activity: DeadlineActivity,
    pub(super) deadline_bandwidth_cpu: Option<CpuId>,
    pub(super) deadline_cleanup_pending: bool,
    pub(super) deadline_bandwidth_scaled: u64,
    pub(super) active_deadline_reservation: u64,
    pub(super) desired_deadline_reservation: u64,
    pub(super) deadline_zero_lag_ns: u64,
    pub(super) queued_cpu: Option<CpuId>,
    pub(super) running_cpu: Option<CpuId>,
    pub(super) on_cpu: Option<CpuId>,
    pub(super) migration_target: Option<CpuId>,
    pub(super) blocked_pi_waiters: usize,
    pub(super) pi_donor: Option<ThreadId>,
    pub(super) deadline_donor: Option<ThreadId>,
    pub(super) deadline_donor_core: Option<Weak<ThreadCore>>,
    pub(super) deadline_cbs_borrower: Option<ThreadId>,
    pub(super) deadline_cbs_generation: u64,
    pub(super) pi_critical_rescue: bool,
    pub(super) deadline_replenish_pending: bool,
    pub(super) deadline_overrun_events: u64,
    pub(super) charged_runtime_ns: u64,
    pub(super) context: ExecutionContextHandle,
    pub(super) address_space: AddressSpaceHandle,
}

impl ThreadSchedState {
    pub(super) fn transition(
        &mut self,
        core: &ThreadCore,
        state: ThreadState,
    ) -> Result<(), TaskError> {
        self.lifecycle.transition(state)?;
        core.publish_state(state);
        Ok(())
    }

    pub(super) fn is_pi_boosted_rt_owner(&self) -> bool {
        self.blocked_pi_waiters != 0
            && self.is_pi_boosted()
            && matches!(
                self.policy,
                SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
            )
    }

    pub(super) const fn is_pi_boosted(&self) -> bool {
        self.pi_donor.is_some()
    }
}
