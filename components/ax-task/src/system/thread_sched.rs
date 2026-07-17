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

/// Scheduler-owned runqueue/current placement, equivalent to Linux `on_rq`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunPlacement {
    Off,
    Queued(CpuId),
    Running(CpuId),
}

/// Architecture execution ownership, equivalent to Linux `on_cpu`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionOwner {
    OffCpu,
    OnCpu(CpuId),
}

/// Orthogonal scheduler and architecture ownership for one thread.
///
/// `Queued + OnCpu` is intentionally representable: an outgoing thread may be
/// requeued before switch tail releases its old stack. No other code may
/// synthesize placement by updating independent optional CPU fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ThreadPlacement {
    run: RunPlacement,
    execution: ExecutionOwner,
}

impl ThreadPlacement {
    pub(super) const DETACHED: Self = Self {
        run: RunPlacement::Off,
        execution: ExecutionOwner::OffCpu,
    };

    const fn queued_cpu(self) -> Option<CpuId> {
        match self.run {
            RunPlacement::Queued(cpu) => Some(cpu),
            RunPlacement::Off | RunPlacement::Running(_) => None,
        }
    }

    const fn running_cpu(self) -> Option<CpuId> {
        match self.run {
            RunPlacement::Running(cpu) => Some(cpu),
            RunPlacement::Off | RunPlacement::Queued(_) => None,
        }
    }

    const fn run_cpu(self) -> Option<CpuId> {
        match self.run {
            RunPlacement::Off => None,
            RunPlacement::Queued(cpu) | RunPlacement::Running(cpu) => Some(cpu),
        }
    }

    const fn on_cpu(self) -> Option<CpuId> {
        match self.execution {
            ExecutionOwner::OffCpu => None,
            ExecutionOwner::OnCpu(cpu) => Some(cpu),
        }
    }

    const fn run_is_off(self) -> bool {
        matches!(self.run, RunPlacement::Off)
    }

    const fn execution_is_off(self) -> bool {
        matches!(self.execution, ExecutionOwner::OffCpu)
    }

    fn mark_queued(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        if !self.run_is_off() {
            return Err(TaskError::AlreadyQueued);
        }
        if let ExecutionOwner::OnCpu(owner) = self.execution
            && owner != cpu
        {
            return Err(TaskError::InvalidConfiguration);
        }
        self.run = RunPlacement::Queued(cpu);
        Ok(())
    }

    fn clear_queued(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        if self.run != RunPlacement::Queued(cpu) {
            return Err(TaskError::InvalidConfiguration);
        }
        self.run = RunPlacement::Off;
        Ok(())
    }

    fn start_running_from_queue(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        if self.run != RunPlacement::Queued(cpu) {
            return Err(TaskError::InvalidConfiguration);
        }
        if let ExecutionOwner::OnCpu(owner) = self.execution
            && owner != cpu
        {
            return Err(TaskError::InvalidConfiguration);
        }
        self.run = RunPlacement::Running(cpu);
        Ok(())
    }

    fn start_running_detached(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        if !self.run_is_off() {
            return Err(TaskError::InvalidConfiguration);
        }
        if let ExecutionOwner::OnCpu(owner) = self.execution
            && owner != cpu
        {
            return Err(TaskError::InvalidConfiguration);
        }
        self.run = RunPlacement::Running(cpu);
        Ok(())
    }

    fn stop_running(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        if self.run != RunPlacement::Running(cpu) {
            return Err(TaskError::InvalidConfiguration);
        }
        self.run = RunPlacement::Off;
        Ok(())
    }

    fn mark_on_cpu(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        if self.run != RunPlacement::Running(cpu) {
            return Err(TaskError::InvalidConfiguration);
        }
        match self.execution {
            ExecutionOwner::OffCpu => self.execution = ExecutionOwner::OnCpu(cpu),
            ExecutionOwner::OnCpu(owner) if owner == cpu => {}
            ExecutionOwner::OnCpu(_) => return Err(TaskError::InvalidConfiguration),
        }
        Ok(())
    }

    fn clear_on_cpu(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        if self.execution != ExecutionOwner::OnCpu(cpu) {
            return Err(TaskError::InvalidConfiguration);
        }
        self.execution = ExecutionOwner::OffCpu;
        Ok(())
    }

    #[cfg(test)]
    fn force_on_cpu(&mut self, cpu: CpuId) {
        self.execution = ExecutionOwner::OnCpu(cpu);
    }
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
                placement: ThreadPlacement::DETACHED,
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
    pub(super) placement: ThreadPlacement,
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
    pub(super) const fn queued_cpu(&self) -> Option<CpuId> {
        self.placement.queued_cpu()
    }

    pub(super) const fn running_cpu(&self) -> Option<CpuId> {
        self.placement.running_cpu()
    }

    pub(super) const fn run_cpu(&self) -> Option<CpuId> {
        self.placement.run_cpu()
    }

    pub(super) const fn on_cpu(&self) -> Option<CpuId> {
        self.placement.on_cpu()
    }

    pub(super) const fn run_is_off(&self) -> bool {
        self.placement.run_is_off()
    }

    pub(super) const fn execution_is_off(&self) -> bool {
        self.placement.execution_is_off()
    }

    pub(super) fn mark_queued(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        self.placement.mark_queued(cpu)
    }

    pub(super) fn clear_queued(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        self.placement.clear_queued(cpu)
    }

    pub(super) fn start_running_from_queue(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        self.placement.start_running_from_queue(cpu)
    }

    pub(super) fn start_running_detached(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        self.placement.start_running_detached(cpu)
    }

    pub(super) fn stop_running(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        self.placement.stop_running(cpu)
    }

    pub(super) fn mark_on_cpu(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        self.placement.mark_on_cpu(cpu)
    }

    pub(super) fn clear_on_cpu(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        self.placement.clear_on_cpu(cpu)
    }

    #[cfg(test)]
    pub(super) fn force_on_cpu(&mut self, cpu: CpuId) {
        self.placement.force_on_cpu(cpu);
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

#[cfg(test)]
mod placement_tests {
    use super::*;

    #[test]
    fn outgoing_self_requeue_remains_on_cpu_until_switch_tail() {
        let cpu = CpuId::new(0);
        let mut placement = ThreadPlacement::DETACHED;
        placement.start_running_detached(cpu).unwrap();
        placement.mark_on_cpu(cpu).unwrap();

        placement.stop_running(cpu).unwrap();
        placement.mark_queued(cpu).unwrap();

        assert_eq!(placement.queued_cpu(), Some(cpu));
        assert_eq!(placement.on_cpu(), Some(cpu));
        placement.clear_on_cpu(cpu).unwrap();
        assert_eq!(placement.on_cpu(), None);
    }

    #[test]
    fn outgoing_context_cannot_be_queued_on_another_cpu() {
        let source = CpuId::new(0);
        let target = CpuId::new(1);
        let mut placement = ThreadPlacement::DETACHED;
        placement.start_running_detached(source).unwrap();
        placement.mark_on_cpu(source).unwrap();
        placement.stop_running(source).unwrap();

        assert_eq!(
            placement.mark_queued(target),
            Err(TaskError::InvalidConfiguration)
        );
        assert!(placement.run_is_off());
        assert_eq!(placement.on_cpu(), Some(source));
    }
}
