//! Membership under the owning scheduler transaction.

use super::*;

impl<'a> OwnerRqTxn<'a> {
    /// Linux-style rq mutation: placement was validated under `p->pi_lock`
    /// before the owner rq transaction began, so a missing entity here is an
    /// ownership violation rather than a recoverable scheduling result.
    pub(crate) fn deactivate_task(&mut self, thread: ThreadId) -> QueuedThread {
        self.scheduler_queue_mut()
            .deactivate_task(thread)
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x5251_1007, thread.as_u64() as usize))
    }

    /// Returns whether this rq currently owns a pushable task of one RT/DL
    /// class. The rq lock held by the transaction is the proof consumed by the
    /// root-domain push callback publication.
    pub(crate) fn has_pushable_class_tasks(&self, class: SchedulingClass) -> bool {
        match class {
            SchedulingClass::Realtime => self.run_queue().has_pushable_realtime(),
            SchedulingClass::Deadline => self.run_queue().has_pushable_deadline(),
            SchedulingClass::Stop | SchedulingClass::Fair => false,
        }
    }

    pub(crate) fn deactivate_unlinked_current(&mut self, thread: ThreadId) {
        self.scheduler_queue_mut()
            .deactivate_unlinked_current(thread);
    }

    pub(crate) fn delay_dequeue_unlinked_current(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
        force: bool,
    ) -> Option<SchedulingEntity> {
        self.run_queue_mut()
            .delay_dequeue_unlinked_current(thread, timing_granularity_ns, force)
    }

    pub(crate) fn is_delayed_fair(&self, thread: ThreadId) -> bool {
        self.run_queue().is_delayed_fair(thread)
    }

    pub(crate) fn finish_delayed_fair_dequeue(
        &mut self,
        thread: ThreadId,
        timing_granularity_ns: u64,
    ) -> QueuedThread {
        self.run_queue_mut()
            .finish_delayed_fair_dequeue(thread, timing_granularity_ns)
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x5251_1014, thread.as_u64() as usize))
    }

    pub(crate) fn reactivate_delayed_fair(
        &mut self,
        thread: ThreadId,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> OwnerRqEnqueue {
        self.run_queue_mut()
            .reactivate_delayed_fair(thread, current_fair, timing_granularity_ns)
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x5251_1015, thread.as_u64() as usize))
    }

    pub(crate) fn throttle_current_deadline(
        &mut self,
        thread: ThreadId,
    ) -> Result<SchedulingEntity, TaskError> {
        self.scheduler_queue_mut().throttle_current_deadline(thread)
    }

    pub(crate) fn replenish_throttled_deadline(
        &mut self,
        thread: ThreadId,
        entity: SchedulingEntity,
    ) -> Result<(), TaskError> {
        self.scheduler_queue_mut()
            .replenish_throttled_deadline(thread, entity)
    }

    /// Unlinks one runnable entity for a class change without changing
    /// `rq->nr_running`.
    pub(crate) fn reclassify_task(&mut self, thread: ThreadId) -> QueuedThread {
        self.scheduler_queue_mut()
            .reclassify_task(thread)
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x5251_1008, thread.as_u64() as usize))
    }

    pub(crate) fn take_delayed_fair_for_update(&mut self, thread: ThreadId) -> QueuedThread {
        self.run_queue_mut()
            .take_delayed_fair_for_update(thread)
            .unwrap_or_else(|| task_runtime::fatal_invariant(0x5251_1016, thread.as_u64() as usize))
    }

    pub(crate) fn restore_delayed_fair_after_update(
        &mut self,
        thread: QueuedThread,
    ) -> SchedulingEntity {
        self.run_queue_mut()
            .restore_delayed_fair_after_update(thread)
    }

    pub(crate) fn finish_detached_delayed_fair(
        &mut self,
        active: &mut ActiveSchedulingState,
        timing_granularity_ns: u64,
    ) {
        self.run_queue_mut()
            .finish_detached_delayed_fair(active, timing_granularity_ns);
    }

    pub(crate) fn enqueue_delayed_fair_transfer(
        &mut self,
        thread: QueuedThread,
        current_fair: Option<FairEntity>,
    ) -> OwnerRqEnqueue {
        let id = thread.id;
        self.run_queue_mut()
            .enqueue_delayed_fair_transfer(thread, current_fair)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x5251_1017, id.as_u64() as usize))
    }

    pub(crate) fn enqueue_reactivated_delayed_fair_transfer(
        &mut self,
        thread: QueuedThread,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> OwnerRqEnqueue {
        let id = thread.id;
        self.run_queue_mut()
            .enqueue_reactivated_delayed_fair_transfer(thread, current_fair, timing_granularity_ns)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x5251_1018, id.as_u64() as usize))
    }

    pub(crate) fn enqueue_task(
        &mut self,
        thread: QueuedThread,
        reason: EnqueueReason,
        current_fair: Option<FairEntity>,
    ) -> OwnerRqEnqueue {
        let id = thread.id;
        self.run_queue_mut()
            .enqueue_task(thread, reason, current_fair)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x5251_1006, id.as_u64() as usize))
    }

    pub(crate) fn enqueue_throttled_deadline(&mut self, thread: QueuedThread) {
        let id = thread.id;
        self.scheduler_queue_mut()
            .enqueue_throttled_deadline(thread)
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x5251_100d, id.as_u64() as usize));
    }

    pub(crate) fn register_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        if !self.run_queue_mut().register_deadline_member(core) {
            task_runtime::fatal_invariant(0x5251_100e, core.id().as_u64() as usize);
        }
    }

    pub(crate) fn unregister_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        self.run_queue_mut().unregister_deadline_member(core);
    }

    pub(crate) fn add_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.run_queue_mut()
            .add_deadline_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn remove_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.run_queue_mut()
            .remove_deadline_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn activate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.run_queue_mut()
            .activate_deadline_bandwidth(utilization_scaled);
    }

    pub(crate) fn deactivate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.run_queue_mut()
            .deactivate_deadline_bandwidth(utilization_scaled);
    }

    pub(crate) fn update_base_deadline_entity(
        &mut self,
        thread: ThreadId,
        entity: SchedulingEntity,
    ) -> bool {
        self.run_queue_mut()
            .update_base_deadline_entity(thread, entity)
    }

    pub(crate) fn begin_balance_scan(&mut self, class: Option<SchedulingClass>) -> BalanceScan {
        self.scheduler_queue_mut().begin_balance_scan(class)
    }

    pub(crate) fn next_balance_candidate(
        &mut self,
        scan: &mut BalanceScan,
        may_migrate: impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThreadSnapshot> {
        self.scheduler_queue_mut()
            .next_balance_candidate(scan, may_migrate)
    }

    pub(crate) fn detach_for_transfer(
        &mut self,
        thread: ThreadId,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> Option<QueuedThread> {
        self.scheduler_queue_mut()
            .detach_for_transfer(thread, current_fair, timing_granularity_ns)
    }
}
