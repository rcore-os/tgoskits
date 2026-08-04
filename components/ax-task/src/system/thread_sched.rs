//! Per-thread scheduler state owned independently from the generation registry.

mod deadline_state;
mod pi_state;
mod placement;
mod policy_state;
mod runtime_state;

use alloc::sync::Weak;

use crate::{
    CpuId, CpuSet, DeadlineEntity, SchedulePolicy, SchedulingEntity, TaskError, ThreadCore,
    ThreadId, ThreadLifecycle, ThreadState,
    lock::{IrqTicketGuard, IrqTicketLock},
    runtime::{AddressSpaceHandle, ExecutionContextHandle},
    timer::TaskDeadlineRegistration,
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

    pub(crate) fn scheduler_fence_cpu(&self) -> Option<CpuId> {
        self.state.lock().placement.on_cpu()
    }

    pub(crate) fn assigned_cpu(&self) -> Option<CpuId> {
        self.state.lock().placement.assigned_cpu()
    }

    #[cfg(test)]
    pub(crate) fn new_test(id: ThreadId, policy: SchedulePolicy) -> Self {
        let entity = SchedulingEntity::new(policy, 1, 0);
        Self::new(
            id,
            ThreadSchedState::new(
                policy,
                entity,
                CpuSet::all(1),
                0,
                ExecutionContextHandle::NONE,
                AddressSpaceHandle::NONE,
            ),
        )
    }
}

#[derive(Debug)]
pub(super) struct ThreadSchedState {
    pub(super) lifecycle: ThreadLifecycle,
    pub(super) policy: policy_state::ThreadPolicyState,
    pub(super) placement: placement::ThreadPlacementState,
    pub(super) deadline: deadline_state::ThreadDeadlineState,
    pub(super) pi: pi_state::ThreadPiState,
    pub(super) runtime: runtime_state::ThreadRuntimeState,
}

impl ThreadSchedState {
    pub(super) const fn new(
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        affinity: CpuSet,
        deadline_reservation: u64,
        context: ExecutionContextHandle,
        address_space: AddressSpaceHandle,
    ) -> Self {
        Self {
            lifecycle: ThreadLifecycle::new(),
            policy: policy_state::ThreadPolicyState::new(policy, entity),
            placement: placement::ThreadPlacementState::new(affinity),
            deadline: deadline_state::ThreadDeadlineState::new(deadline_reservation),
            pi: pi_state::ThreadPiState::new(),
            runtime: runtime_state::ThreadRuntimeState::new(context, address_space),
        }
    }

    pub(super) fn transition(
        &mut self,
        core: &ThreadCore,
        state: ThreadState,
    ) -> Result<(), TaskError> {
        self.lifecycle.transition(state)?;
        core.publish_state(state);
        Ok(())
    }

    pub(super) fn throttle_ready_deadline(&mut self, core: &ThreadCore) -> Result<(), TaskError> {
        self.lifecycle.throttle_ready_deadline()?;
        core.publish_state(ThreadState::Blocked);
        Ok(())
    }

    pub(super) fn is_pi_boosted_rt_owner(&self) -> bool {
        self.pi.blocked_waiters != 0
            && self.is_pi_boosted()
            && matches!(
                self.policy.effective,
                SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
            )
    }

    pub(super) const fn is_pi_boosted(&self) -> bool {
        self.pi.donor.is_some()
    }
}
