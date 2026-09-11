//! Observation under the owning scheduler transaction.

use super::*;

impl<'a> OwnerRqTxn<'a> {
    pub(crate) fn current(&self) -> Option<&CurrentDispatch> {
        self.run_queue().current()
    }

    pub(crate) fn current_mut(&mut self) -> Option<&mut CurrentDispatch> {
        self.run_queue_mut().current_mut()
    }

    pub(crate) fn current_scheduling_entity(&self) -> Option<&SchedulingEntity> {
        self.run_queue().current_scheduling_entity()
    }

    /// Returns the current class urgency from the rq-owned scheduling state.
    pub(crate) fn current_scheduling_urgency(&self) -> Option<SchedulingUrgency> {
        let policy = self.current()?.schedule_policy();
        if matches!(policy, SchedulePolicy::Deadline(_)) {
            self.current_scheduling_entity()
                .map(|entity| entity.scheduling_urgency(policy))
        } else {
            Some(policy.scheduling_urgency())
        }
    }

    pub(crate) fn current_fair_contender(&self) -> Option<FairEntity> {
        self.run_queue().current_fair_contender()
    }

    pub(crate) fn current_scheduling_entity_mut(&mut self) -> Option<&mut SchedulingEntity> {
        self.run_queue_mut().current_scheduling_entity_mut()
    }

    pub(crate) fn linked_current_entity_mut(
        &mut self,
        thread: ThreadId,
    ) -> Option<&mut SchedulingEntity> {
        self.run_queue_mut().linked_current_entity_mut(thread)
    }

    pub(crate) fn update_fair_virtual_time(&mut self, current: Option<FairEntity>) {
        self.scheduler_queue_mut().update_fair_virtual_time(current);
    }

    pub(crate) fn wakeup_preempt(
        &mut self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: &SchedulingEntity,
        fair_virtual_time: u64,
    ) -> WakePreemptionDecision {
        self.run_queue_mut()
            .wakeup_preempt(wakee, policy, entity, fair_virtual_time)
    }

    pub(crate) fn wakeup_preempt_with_intent(
        &mut self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: &SchedulingEntity,
        fair_virtual_time: u64,
        context: WakePreemptionContext,
    ) -> WakePreemptionDecision {
        self.run_queue_mut().wakeup_preempt_with_intent(
            wakee,
            policy,
            entity,
            fair_virtual_time,
            context,
        )
    }

    pub(crate) fn capture_current_fair_migration(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
    ) {
        self.run_queue_mut()
            .capture_current_fair_migration(thread, timing_granularity_ns);
    }

    pub(crate) fn current_thread(&self) -> Option<ThreadId> {
        self.run_queue().current_thread()
    }

    /// Samples the task's Linux rq facts while this transaction owns the rq.
    pub(in crate::sched::system) fn task_state(
        &self,
        thread: ThreadId,
        placement: &SchedulerPlacement,
    ) -> OwnerRqTaskState {
        let owner = self.owner();
        let queued = placement.queued_cpu() == Some(owner);
        let on_cpu = placement.on_cpu() == Some(owner);
        if self.current_thread() == Some(thread) {
            if !queued || !on_cpu {
                task_runtime::fatal_invariant(0x5251_1011, thread.as_u64() as usize);
            }
            OwnerRqTaskState::Current
        } else if queued && self.is_delayed_fair(thread) {
            OwnerRqTaskState::DelayedFair { outgoing: on_cpu }
        } else if queued {
            OwnerRqTaskState::Queued { outgoing: on_cpu }
        } else {
            OwnerRqTaskState::Inactive
        }
    }

    pub(crate) fn current_core(&self) -> Option<Arc<ThreadCore>> {
        self.run_queue().current_core()
    }

    pub(crate) fn current_core_ref(&self) -> Option<&ThreadCore> {
        self.run_queue().current_core_ref()
    }

    pub(crate) fn current_switch_endpoint(&self) -> Option<SwitchEndpoint> {
        self.run_queue().current_switch_endpoint()
    }

    pub(crate) fn update_current_runtime_binding(
        &mut self,
        thread: ThreadId,
        binding: crate::runtime::switch::ThreadRuntimeBinding,
        membarrier_state: crate::runtime::resource::AddressSpaceMembarrierState,
    ) {
        self.run_queue_mut()
            .update_current_runtime_binding(thread, binding, membarrier_state)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1003, thread.as_u64() as usize)
            });
    }
}
