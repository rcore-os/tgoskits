//! Membership under the owning scheduler transaction.

use super::*;

impl CpuRunQueueState {
    pub(in crate::sched::system::cpu) fn enqueue_task(
        &mut self,
        thread: QueuedThread,
        reason: EnqueueReason,
        current_fair: Option<FairEntity>,
    ) -> Result<OwnerRqEnqueue, TaskError> {
        // Linux RT enqueue does not alter the current task's runtime
        // deadline. Avoid deriving Fair/EEVDF clockevent state for the common
        // FIFO/RR wake while keeping the same class enqueue and rq accounting.
        let realtime = thread.active.policy().rt_priority().is_some();
        let runtime_timer_required_before = if realtime {
            false
        } else {
            self.current_runtime_timer_required()
        };
        let runtime_timer_delta_before = (!realtime)
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        let entity = self.queue.enqueue_task(thread, reason, current_fair)?;
        if !realtime {
            self.tighten_current_fair_slice_protection(&entity);
        }
        let runtime_timer_required_after = if realtime {
            false
        } else {
            self.current_runtime_timer_required()
        };
        let runtime_timer_delta_after = (!realtime)
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        Ok(OwnerRqEnqueue {
            entity,
            scheduler_deadline_refresh_required: runtime_timer_required_after
                && (!runtime_timer_required_before
                    || runtime_timer_delta_after < runtime_timer_delta_before),
        })
    }

    pub(in crate::sched::system::cpu) fn take_delayed_fair_for_update(
        &mut self,
        thread: ThreadId,
    ) -> Option<QueuedThread> {
        let current_fair = self.current_fair_contender();
        self.queue.update_fair_virtual_time(current_fair);
        let delayed = self.queue.take_delayed_fair_for_update(thread)?;
        self.queue.update_fair_virtual_time(current_fair);
        Some(delayed)
    }

    pub(in crate::sched::system::cpu) fn restore_delayed_fair_after_update(
        &mut self,
        thread: QueuedThread,
    ) -> SchedulingEntity {
        let current_fair = self.current_fair_contender();
        self.queue.update_fair_virtual_time(current_fair);
        let entity = self.queue.restore_delayed_fair_after_update(thread);
        self.tighten_current_fair_slice_protection(&entity);
        self.queue.update_fair_virtual_time(current_fair);
        entity
    }

    pub(in crate::sched::system::cpu) fn finish_detached_delayed_fair(
        &mut self,
        active: &mut ActiveSchedulingState,
        timing_granularity_ns: u64,
    ) {
        let current_fair = self.current_fair_contender();
        self.queue
            .finish_detached_delayed_fair(active, timing_granularity_ns);
        self.queue.update_fair_virtual_time(current_fair);
    }

    pub(in crate::sched::system::cpu) fn enqueue_delayed_fair_transfer(
        &mut self,
        thread: QueuedThread,
        current_fair: Option<FairEntity>,
    ) -> Result<OwnerRqEnqueue, TaskError> {
        let runtime_timer_required_before = self.current_runtime_timer_required();
        let runtime_timer_delta_before = runtime_timer_required_before
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        let entity = self
            .queue
            .enqueue_delayed_fair_transfer(thread, current_fair)?;
        self.tighten_current_fair_slice_protection(&entity);
        let runtime_timer_required_after = self.current_runtime_timer_required();
        let runtime_timer_delta_after = runtime_timer_required_after
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        Ok(OwnerRqEnqueue {
            entity,
            scheduler_deadline_refresh_required: runtime_timer_required_before
                != runtime_timer_required_after
                || runtime_timer_delta_before != runtime_timer_delta_after,
        })
    }

    pub(in crate::sched::system::cpu) fn enqueue_reactivated_delayed_fair_transfer(
        &mut self,
        thread: QueuedThread,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> Result<OwnerRqEnqueue, TaskError> {
        let runtime_timer_required_before = self.current_runtime_timer_required();
        let runtime_timer_delta_before = runtime_timer_required_before
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        let entity = self.queue.enqueue_reactivated_delayed_fair_transfer(
            thread,
            current_fair,
            timing_granularity_ns,
        )?;
        self.tighten_current_fair_slice_protection(&entity);
        let runtime_timer_required_after = self.current_runtime_timer_required();
        let runtime_timer_delta_after = runtime_timer_required_after
            .then(|| self.current_runtime_timer_delta_ns())
            .flatten();
        Ok(OwnerRqEnqueue {
            entity,
            scheduler_deadline_refresh_required: runtime_timer_required_after
                && (!runtime_timer_required_before
                    || runtime_timer_delta_after < runtime_timer_delta_before),
        })
    }

    pub(super) fn tighten_current_fair_slice_protection(
        &mut self,
        wakee_entity: &SchedulingEntity,
    ) {
        if !wakee_entity
            .fair()
            .is_some_and(|fair| fair.mode() == FairMode::Normal)
        {
            return;
        }
        let Some(shortest_queued_slice_ns) = self.queue.min_fair_service_request_ns() else {
            return;
        };
        let Some(current) = self
            .current_scheduling_entity_mut()
            .and_then(|entity| match entity {
                SchedulingEntity::Fair(fair) => Some(fair),
                _ => None,
            })
        else {
            return;
        };
        current.update_slice_protection(shortest_queued_slice_ns);
    }
}
