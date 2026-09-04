use super::*;
use crate::{ActiveSchedulingState, runtime::task_runtime};

impl RunQueue {
    /// Linux Fair `dequeue_task(..., DEQUEUE_SLEEP)` returning false.
    ///
    /// The outgoing current is inserted into its Fair tree with explicit
    /// delayed state while common `rq->nr_running` remains unchanged.
    pub(crate) fn enqueue_delayed_fair_current(&mut self, entry: QueuedThread) -> SchedulingEntity {
        self.link_fair(entry, true)
    }

    fn link_fair(&mut self, mut entry: QueuedThread, delayed: bool) -> SchedulingEntity {
        if self.contains(entry.id) {
            task_runtime::fatal_invariant(0x5251_1012, entry.id.as_u64() as usize);
        }
        entry.sequence = self.allocate_sequence();
        let id = entry.id;
        let policy = entry.active.policy();
        let entity = entry.active.entity().clone();
        match policy {
            SchedulePolicy::Fair { .. } => {
                // Normal, Batch, and SCHED_IDLE all link into Linux's one
                // EEVDF cfs_rq.
                if delayed {
                    self.fair.insert_delayed(entry);
                } else {
                    self.fair.insert(entry);
                }
                self.register_membership(id, QueueMembershipClass::Fair);
            }
            _ => task_runtime::fatal_invariant(0x5251_1013, id.as_u64() as usize),
        }
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_add(fixed_placement_demand(policy));
        self.refresh_class_pushable(id, self.linked_current());
        self.mark_publication_dirty();
        entity
    }

    pub(crate) fn is_delayed_fair(&self, id: ThreadId) -> bool {
        match self.membership_class(id) {
            Some(QueueMembershipClass::Fair) => self.fair.is_delayed(id),
            _ => false,
        }
    }

    /// Linux `sched_change` SAVE dequeue for an on-rq delayed Fair task.
    pub(crate) fn take_delayed_fair_for_update(&mut self, id: ThreadId) -> Option<QueuedThread> {
        let class = self.membership_class(id)?;
        let thread = match class {
            QueueMembershipClass::Fair => self.fair.take_delayed(id)?,
            _ => return None,
        };
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_sub(fixed_placement_demand(thread.active.policy()));
        self.unregister_membership(id);
        self.refresh_class_pushable(id, self.linked_current());
        self.mark_publication_dirty();
        Some(thread)
    }

    /// Restores a same-class delayed Fair task after policy or PI reweighting.
    pub(crate) fn restore_delayed_fair_after_update(
        &mut self,
        thread: QueuedThread,
    ) -> SchedulingEntity {
        self.link_fair(thread, true)
    }

    /// Completes Linux `switching_from_fair()` after a delayed class boundary.
    pub(crate) fn finish_detached_delayed_fair(
        &mut self,
        active: &mut ActiveSchedulingState,
        timing_granularity_ns: u64,
    ) {
        if let SchedulingEntity::Fair(fair) = active.base_entity_mut()
            && fair.is_delayed()
        {
            let virtual_time = self.virtual_time();
            let rq_max_slice_ns = self
                .max_fair_service_request_ns()
                .unwrap_or(fair.service_request_ns())
                .max(fair.service_request_ns());
            fair.finish_delayed_dequeue(virtual_time, rq_max_slice_ns, timing_granularity_ns)
                .expect("detached delayed Fair state must finish exactly once");
        }
        self.nr_running = self
            .nr_running
            .checked_sub(1)
            .expect("delayed class boundary must remove one rq->nr_running entity");
        self.mark_publication_dirty();
    }

    /// Installs a blocked delayed Fair entity transferred from another rq.
    pub(crate) fn enqueue_delayed_fair_transfer(
        &mut self,
        mut thread: QueuedThread,
        current_fair: Option<FairEntity>,
    ) -> Result<SchedulingEntity, TaskError> {
        let SchedulingEntity::Fair(fair) = thread.active.entity_mut() else {
            return Err(TaskError::InvalidConfiguration);
        };
        let virtual_time = self.virtual_time();
        let (queue_weight, current_weight) = self.fair_placement_weights(current_fair);
        fair.place_delayed_after_transfer(
            virtual_time,
            queue_weight.saturating_add(current_weight),
        )?;
        self.nr_running = self
            .nr_running
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        Ok(self.link_fair(thread, true))
    }

    /// Completes Linux `ENQUEUE_DELAYED` after wake overtakes an rq transfer.
    pub(crate) fn enqueue_reactivated_delayed_fair_transfer(
        &mut self,
        mut thread: QueuedThread,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> Result<SchedulingEntity, TaskError> {
        let SchedulingEntity::Fair(fair) = thread.active.entity_mut() else {
            return Err(TaskError::InvalidConfiguration);
        };
        let id = thread.id;
        let virtual_time = self.virtual_time();
        let (queue_weight, current_weight) = self.fair_placement_weights(current_fair);
        fair.place_delayed_after_transfer(
            virtual_time,
            queue_weight.saturating_add(current_weight),
        )?;
        self.nr_running = self
            .nr_running
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        self.link_fair(thread, true);
        self.update_fair_virtual_time(current_fair);
        let entity = self
            .reactivate_delayed_fair(id, current_fair, timing_granularity_ns)
            .expect("a newly linked delayed Fair transfer must reactivate");
        Ok(entity)
    }

    /// Physically removes a Fair sleeper selected after delayed dequeue.
    pub(crate) fn finish_delayed_fair_dequeue(
        &mut self,
        id: ThreadId,
        timing_granularity_ns: u64,
    ) -> Option<QueuedThread> {
        if self.membership_class(id)? != QueueMembershipClass::Fair {
            return None;
        }
        let virtual_time = self.fair.virtual_time();
        let thread = self
            .fair
            .finish_delayed_dequeue(id, virtual_time, timing_granularity_ns)?;
        self.nr_running = self
            .nr_running
            .checked_sub(1)
            .expect("delayed dequeue must remove one rq->nr_running entity");
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_sub(fixed_placement_demand(thread.active.policy()));
        self.unregister_membership(id);
        self.refresh_class_pushable(id, self.linked_current());
        self.update_fair_virtual_time(None);
        self.mark_publication_dirty();
        Some(thread)
    }

    /// Linux `enqueue_task(..., ENQUEUE_DELAYED)`.
    pub(crate) fn reactivate_delayed_fair(
        &mut self,
        id: ThreadId,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> Option<SchedulingEntity> {
        if self.membership_class(id)? != QueueMembershipClass::Fair {
            return None;
        }
        let entity = self
            .fair
            .reactivate_delayed(id, current_fair, timing_granularity_ns)?;
        self.refresh_class_pushable(id, self.linked_current());
        self.mark_publication_dirty();
        Some(entity)
    }

    pub(crate) fn enqueue_task(
        &mut self,
        mut entry: QueuedThread,
        reason: EnqueueReason,
        current_fair: Option<FairEntity>,
    ) -> Result<SchedulingEntity, TaskError> {
        if self.contains(entry.id) {
            return Err(TaskError::AlreadyQueued);
        }
        entry.sequence = self.allocate_sequence();
        let id = entry.id;
        let policy = entry.active.policy();
        let class_enqueue =
            SchedulerClass::for_policy(policy).enqueue_task(self, entry, reason, current_fair)?;
        let membership_class = class_enqueue.membership;
        let queued_entity = class_enqueue.entity;
        let reason = class_enqueue.reason;
        if matches!(
            reason,
            EnqueueReason::Wake | EnqueueReason::Replenished | EnqueueReason::Migrated
        ) {
            self.nr_running += 1;
        }
        if !matches!(reason, EnqueueReason::Preempted | EnqueueReason::Yield) {
            self.fixed_placement_demand = self
                .fixed_placement_demand
                .saturating_add(fixed_placement_demand(policy));
        }
        self.register_membership(id, membership_class);
        self.refresh_class_pushable(id, self.linked_current());
        if !matches!(reason, EnqueueReason::Preempted | EnqueueReason::Yield) {
            self.mark_publication_dirty();
        }
        Ok(queued_entity)
    }

    /// Activates an already-throttled Deadline task without linking it into
    /// the EDF tree. Linux publishes `TASK_ON_RQ_QUEUED` in this state while
    /// leaving the task out of `rq->nr_running` until the CBS timer fires.
    pub(crate) fn enqueue_throttled_deadline(
        &mut self,
        mut thread: QueuedThread,
    ) -> Result<(), TaskError> {
        if self.contains(thread.id)
            || !matches!(thread.active.policy(), SchedulePolicy::Deadline(_))
            || !thread.active.entity().is_deadline_throttled()
        {
            return Err(TaskError::InvalidConfiguration);
        }
        thread.sequence = self.allocate_sequence();
        let id = thread.id;
        self.deadline.install_throttled(thread)?;
        self.register_membership(id, QueueMembershipClass::DeadlineThrottled);
        Ok(())
    }

    /// Linux `update_curr_dl()` throttle transition for the linked current.
    pub(crate) fn throttle_current_deadline(
        &mut self,
        id: ThreadId,
    ) -> Result<SchedulingEntity, TaskError> {
        if self.linked_current() != Some(id) {
            return Err(TaskError::NotReady);
        }
        let QueueMembershipClass::Deadline(key) =
            self.membership_class(id).ok_or(TaskError::NotReady)?
        else {
            return Err(TaskError::InvalidConfiguration);
        };
        self.retain_current_runtime_core_before_unlink(id);
        let thread = self.deadline.remove(key).ok_or(TaskError::NotReady)?;
        let policy = thread.active.policy();
        let entity = thread.active.entity().clone();
        self.deadline.install_throttled(thread)?;
        self.replace_membership_class(id, QueueMembershipClass::DeadlineThrottled);
        self.nr_running = self
            .nr_running
            .checked_sub(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_sub(fixed_placement_demand(policy));
        self.mark_publication_dirty();
        Ok(entity)
    }

    /// Re-enables one throttled CBS entity after its hard replenishment timer.
    pub(crate) fn replenish_throttled_deadline(
        &mut self,
        id: ThreadId,
        entity: SchedulingEntity,
    ) -> Result<(), TaskError> {
        if !matches!(
            self.membership_class(id),
            Some(QueueMembershipClass::DeadlineThrottled)
        ) || entity.is_deadline_throttled()
        {
            return Err(TaskError::NotReady);
        }
        let mut thread = self
            .deadline
            .take_throttled(id)
            .ok_or(TaskError::NotReady)?;
        *thread.active.entity_mut() = entity;
        let policy = thread.active.policy();
        let key = self.deadline.insert(thread);
        self.replace_membership_class(id, QueueMembershipClass::Deadline(key));
        self.nr_running = self
            .nr_running
            .checked_add(1)
            .ok_or(TaskError::InvalidConfiguration)?;
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_add(fixed_placement_demand(policy));
        self.deadline.refresh_pushable(id, self.linked_current());
        self.mark_publication_dirty();
        Ok(())
    }

    pub(crate) fn is_deadline_throttled_member(&self, id: ThreadId) -> bool {
        self.membership_class(id) == Some(QueueMembershipClass::DeadlineThrottled)
    }

    pub(super) fn fair_placement_weights(&self, current_fair: Option<FairEntity>) -> (u64, u64) {
        let queue_weight = self.fair.total_weight();
        let current_weight = current_fair.map_or(0, |current| u64::from(current.weight()));
        (queue_weight, current_weight)
    }

    pub(super) fn mark_balance_candidate(&mut self, id: ThreadId, scan_epoch: u64) {
        match self
            .membership_class(id)
            .expect("a selected balance candidate must remain queued")
        {
            QueueMembershipClass::Stop => {
                unreachable!("the per-CPU stopper must never be a balance candidate")
            }
            QueueMembershipClass::Deadline(key) => {
                self.deadline
                    .get_mut(key)
                    .expect("deadline balance candidate must remain linked")
                    .balance_scan_epoch = scan_epoch;
            }
            QueueMembershipClass::DeadlineThrottled => {
                unreachable!("a throttled Deadline task is not a push candidate")
            }
            QueueMembershipClass::Realtime(key) => {
                self.rt
                    .get_mut(key)
                    .expect("RT balance candidate must remain linked")
                    .balance_scan_epoch = scan_epoch;
            }
            QueueMembershipClass::Fair => {
                assert!(self.fair.mark_balance_candidate(id, scan_epoch));
            }
        }
    }

    fn unlink_task(&mut self, id: ThreadId, deactivate: bool) -> Option<QueuedThread> {
        let class = self.membership_class(id)?;
        if class == QueueMembershipClass::DeadlineThrottled {
            let thread = self.deadline.take_throttled(id)?;
            self.unregister_membership(id);
            self.mark_publication_dirty();
            return Some(thread);
        }
        self.retain_current_runtime_core_before_unlink(id);
        let scheduler_class = class.scheduler_class();
        let removed = scheduler_class
            .dequeue_task(self, class, id)
            .expect("runqueue membership must identify a linked scheduling entity");
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_sub(fixed_placement_demand(removed.active.policy()));
        self.unregister_membership(removed.id);
        self.refresh_class_pushable(removed.id, self.linked_current());
        if deactivate {
            self.nr_running = self
                .nr_running
                .checked_sub(1)
                .expect("deactivate_task must match one runnable entity");
        }
        self.mark_publication_dirty();
        Some(removed)
    }

    fn retain_current_runtime_core_before_unlink(&mut self, id: ThreadId) {
        if self.linked_current() != Some(id) {
            return;
        }
        let runtime_core = Arc::clone(
            &self
                .linked_current_thread(id)
                .expect("linked current membership must identify its rq node")
                .core,
        );
        self.current
            .as_mut()
            .expect("linked current membership must match rq->curr")
            .retain_runtime_core_before_unlink(runtime_core);
    }

    /// Linux `deactivate_task()`: removes one runnable entity from `nr_running`.
    pub(crate) fn deactivate_task(&mut self, id: ThreadId) -> Option<QueuedThread> {
        self.unlink_task(id, true)
    }

    /// Linux scheduler-class change: unlinks an entity without deactivating it.
    pub(crate) fn reclassify_task(&mut self, id: ThreadId) -> Option<QueuedThread> {
        let was_throttled =
            self.membership_class(id) == Some(QueueMembershipClass::DeadlineThrottled);
        let thread = self.unlink_task(id, false)?;
        if was_throttled {
            // The replacement class becomes eligible immediately. Common
            // PolicyChanged enqueue preserves `nr_running`, so establish the
            // runnable count here just as Linux dequeues a throttled DL class
            // before installing the new scheduler class.
            self.nr_running = self.nr_running.checked_add(1)?;
        }
        Some(thread)
    }

    /// Returns whether `id` is the RT/DL entity retained as current.
    pub(crate) fn is_linked_current(&self, id: ThreadId) -> bool {
        self.linked_current() == Some(id)
    }

    pub(crate) fn linked_current_thread(&self, id: ThreadId) -> Option<&QueuedThread> {
        if self.linked_current() != Some(id) {
            return None;
        }
        match self.membership_class(id)? {
            QueueMembershipClass::Deadline(key) => self.deadline.get(key),
            QueueMembershipClass::Realtime(key) => self.rt.get(key),
            _ => None,
        }
    }

    pub(crate) fn linked_current_entity_mut(
        &mut self,
        id: ThreadId,
    ) -> Option<&mut SchedulingEntity> {
        if self.linked_current() != Some(id) {
            return None;
        }
        match self.membership_class(id)? {
            QueueMembershipClass::Deadline(key) => {
                Some(self.deadline.get_mut(key)?.active.entity_mut())
            }
            QueueMembershipClass::Realtime(key) => Some(self.rt.get_mut(key)?.active.entity_mut()),
            _ => None,
        }
    }

    pub(crate) fn capture_linked_fair_migration(
        &mut self,
        id: ThreadId,
        virtual_time: u64,
        rq_max_slice_ns: u64,
        timing_granularity_ns: u64,
    ) -> bool {
        let active = match self.membership_class(id) {
            Some(QueueMembershipClass::Deadline(key)) => {
                self.deadline.get_mut(key).map(|thread| &mut thread.active)
            }
            Some(QueueMembershipClass::Realtime(key)) => {
                self.rt.get_mut(key).map(|thread| &mut thread.active)
            }
            _ => None,
        };
        let Some(active) = active else {
            return false;
        };
        active.base_entity_mut().capture_fair_migration(
            virtual_time,
            rq_max_slice_ns,
            timing_granularity_ns,
        );
        true
    }

    pub(crate) fn linked_current_entity(&self, id: ThreadId) -> Option<&SchedulingEntity> {
        if self.linked_current() != Some(id) {
            return None;
        }
        match self.membership_class(id)? {
            QueueMembershipClass::Deadline(key) => self.deadline.get(key).map(QueuedThread::entity),
            QueueMembershipClass::Realtime(key) => self.rt.get(key).map(QueuedThread::entity),
            _ => None,
        }
    }

    /// Rebuilds the active EDF key after Linux-style boosted replenishment.
    pub(crate) fn requeue_replenished_deadline_current(
        &mut self,
        id: ThreadId,
    ) -> Result<bool, TaskError> {
        if self.linked_current() != Some(id) {
            return Err(TaskError::NotReady);
        }
        let QueueMembershipClass::Deadline(key) =
            self.membership_class(id).ok_or(TaskError::NotReady)?
        else {
            return Err(TaskError::InvalidConfiguration);
        };
        let (new_key, _entity) = self
            .deadline
            .put_prev_current(key)
            .ok_or(TaskError::NotReady)?;
        self.replace_membership_class(id, QueueMembershipClass::Deadline(new_key));
        self.mark_publication_dirty();
        Ok(self.deadline.first().is_some_and(|thread| thread.id != id))
    }

    pub(crate) fn scheduling_entity(&self, id: ThreadId) -> Option<SchedulingEntity> {
        self.queued_thread_including_current(id)
            .map(|thread| thread.entity)
    }

    pub(crate) fn base_scheduling_entity(&self, id: ThreadId) -> Option<SchedulingEntity> {
        self.queued_thread_including_current(id)
            .map(|thread| thread.base_entity)
    }

    pub(crate) fn scheduling_state(
        &self,
        id: ThreadId,
    ) -> Option<(SchedulePolicy, SchedulingEntity)> {
        self.queued_thread_including_current(id)
            .map(|thread| (thread.policy, thread.entity))
    }

    /// Installs a newly applied RT/DL policy as the physically linked current.
    pub(crate) fn link_running(&mut self, thread: QueuedThread) -> Result<(), TaskError> {
        if self
            .linked_current()
            .is_some_and(|current| self.contains(current))
            || !retains_running_link(thread.active.policy())
        {
            return Err(TaskError::InvalidConfiguration);
        }
        let id = thread.id;
        self.enqueue_task(thread, EnqueueReason::PolicyChanged, None)?;
        self.refresh_class_pushable(id, Some(id));
        Ok(())
    }
}
