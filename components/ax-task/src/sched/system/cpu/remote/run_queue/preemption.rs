//! Preemption under the owning scheduler transaction.

use super::*;

impl CpuRunQueueState {
    /// Applies Linux EEVDF wakeup preemption to the complete owner runqueue.
    ///
    /// Dedicated idle is preempted before any class-local selection rule,
    /// matching Linux's unconditional idle-class `resched_curr()`. Otherwise,
    /// a fair wakee may request rescheduling only when it both defeats the
    /// protected current request and is itself the earliest eligible queued
    /// entity. Comparing only the wakee with current creates needless
    /// reschedule IPIs when an older queued contender would be selected.
    pub(crate) fn wakeup_preempt(
        &mut self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: &SchedulingEntity,
        fair_virtual_time: u64,
    ) -> WakePreemptionDecision {
        self.wakeup_preempt_with_intent(
            wakee,
            policy,
            entity,
            fair_virtual_time,
            WakePreemptionContext::normal(),
        )
    }

    /// Applies wakeup preemption while preserving Linux wake flags.
    pub(crate) fn wakeup_preempt_with_intent(
        &mut self,
        wakee: ThreadId,
        policy: SchedulePolicy,
        entity: &SchedulingEntity,
        fair_virtual_time: u64,
        context: WakePreemptionContext,
    ) -> WakePreemptionDecision {
        let Some(current) = self.current() else {
            return WakePreemptionDecision::WakeeSelected;
        };
        // Linux wakeup_preempt_fair() leaves an existing TIF_NEED_RESCHED
        // request unchanged. Lazy Fair rescheduling and owner-only work are
        // distinct facts and therefore never set this context bit.
        if matches!(policy, SchedulePolicy::Fair { .. }) && context.reschedule_pending {
            return WakePreemptionDecision::KeepCurrent;
        }
        if current.is_dedicated_idle() {
            return WakePreemptionDecision::DedicatedIdlePreempted;
        }
        let current_policy = current.schedule_policy();
        // Linux's `check_preempt_equal_prio()` has already established that
        // an equal-priority RT wake must preserve FIFO order on this rq. The
        // class hook cannot preempt an equal RT task, so avoid cloning the
        // current scheduling entity and re-running the generic class chain.
        // This is the common pinned SCHED_FIFO/RR wake path; the context still
        // carries migration and pending-reschedule facts for the exceptional
        // requeue case handled below.
        if context.equal_rt_action == EqualRtWakeAction::PreserveFifoOrder
            && policy.rt_priority().is_some()
            && policy.rt_priority() == current_policy.rt_priority()
        {
            return WakePreemptionDecision::KeepCurrent;
        }
        if context.equal_rt_action == EqualRtWakeAction::RequeueWakeeAndReschedule {
            if policy.rt_priority() != current_policy.rt_priority()
                || policy.rt_priority().is_none()
                || !self.queue.requeue_realtime_wakee_head(wakee)
            {
                task_runtime::fatal_invariant(0x5251_0004, wakee.as_u64() as usize);
            }
            return WakePreemptionDecision::WakeeSelected;
        }
        let current_entity = self
            .current_scheduling_entity()
            .cloned()
            .expect("current dispatch must have one rq-owned scheduling entity");

        let preempts = if context.intent.is_sync() {
            crate::sched::algorithm::default_sync_wakeup_preempts(
                current_policy,
                &current_entity,
                false,
                policy,
                entity,
                fair_virtual_time,
            )
        } else {
            crate::sched::algorithm::wakeup_preempts(
                current_policy,
                &current_entity,
                false,
                policy,
                entity,
                fair_virtual_time,
            )
        };
        if !preempts {
            return WakePreemptionDecision::KeepCurrent;
        }
        let decision = match policy {
            SchedulePolicy::Fair { .. } => {
                if self.queue.fair_wakee_is_selected(wakee, fair_virtual_time) {
                    WakePreemptionDecision::WakeeSelected
                } else {
                    WakePreemptionDecision::QueuedCandidateSelected
                }
            }
            _ => WakePreemptionDecision::WakeeSelected,
        };
        if decision == WakePreemptionDecision::WakeeSelected
            && fair_preemption_cancels_protection(current_policy, &current_entity, policy, entity)
            && let Some(SchedulingEntity::Fair(current)) = self.current_scheduling_entity_mut()
        {
            current.cancel_slice_protection();
        }
        decision
    }
}
