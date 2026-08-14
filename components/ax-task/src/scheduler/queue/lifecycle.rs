use super::*;

impl RunQueue {
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
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_add(fixed_placement_demand(policy));
        self.register_membership(id, membership_class);
        self.refresh_class_pushable(id, policy, self.linked_current());
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
        let thread = self.deadline.remove(key).ok_or(TaskError::NotReady)?;
        let entity = thread.active.entity().clone();
        self.deadline.install_throttled(thread)?;
        self.replace_membership_class(id, QueueMembershipClass::DeadlineThrottled);
        self.nr_running = self
            .nr_running
            .checked_sub(1)
            .ok_or(TaskError::InvalidConfiguration)?;
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
        Ok(())
    }

    pub(crate) fn is_deadline_throttled_member(&self, id: ThreadId) -> bool {
        self.membership_class(id) == Some(QueueMembershipClass::DeadlineThrottled)
    }

    pub(super) fn fair_placement_weights(
        &self,
        fair: FairEntity,
        current_fair: Option<FairEntity>,
    ) -> (u64, u64) {
        let queue_weight = if fair.mode() == FairMode::Idle {
            self.idle_fair.total_weight()
        } else {
            self.fair.total_weight()
        };
        let current_weight = current_fair
            .filter(|current| current.mode() == fair.mode())
            .map_or(0, |current| u64::from(current.weight()));
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
            QueueMembershipClass::Realtime(priority) => {
                self.rt
                    .get_mut(priority, id)
                    .expect("RT balance candidate must remain linked")
                    .balance_scan_epoch = scan_epoch;
            }
            QueueMembershipClass::Fair => {
                let mut thread = self
                    .fair
                    .remove(id)
                    .expect("fair balance candidate must remain linked");
                thread.balance_scan_epoch = scan_epoch;
                self.fair.insert(thread);
            }
            QueueMembershipClass::IdleFair => {
                let mut thread = self
                    .idle_fair
                    .remove(id)
                    .expect("idle-fair balance candidate must remain linked");
                thread.balance_scan_epoch = scan_epoch;
                self.idle_fair.insert(thread);
            }
        }
    }

    #[cfg(test)]
    pub(super) fn enqueue_test(
        &mut self,
        id: ThreadId,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        _now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<SchedulingEntity, TaskError> {
        self.prepare_thread_slot(id.slot() as usize);
        let sched = Arc::new(crate::ThreadSchedCell::new_test(id, policy));
        let core = Arc::new(ThreadCore::new(id, policy, sched, None, None, None));
        let already_runnable = matches!(reason, EnqueueReason::Preempted);
        let entity = self.enqueue_task(
            QueuedThread::new(
                id,
                ActiveSchedulingState::new(policy, entity),
                core,
                false,
                true,
                RqTaskMetadata::test(1),
            ),
            reason,
            None,
        )?;
        if already_runnable {
            // Production reaches `Preempted` from a Fair current which is
            // already included in rq->nr_running. Unit tests inject that
            // post-put-prev state directly, so establish the same common-rq
            // accounting without reapplying the wake placement rule.
            self.nr_running = self
                .nr_running
                .checked_add(1)
                .ok_or(TaskError::InvalidConfiguration)?;
        }
        Ok(entity)
    }

    #[cfg(test)]
    pub(super) fn enqueue_rt_test(
        &mut self,
        id: ThreadId,
        policy: SchedulePolicy,
        quota_exempt: bool,
    ) -> Result<SchedulingEntity, TaskError> {
        self.prepare_thread_slot(id.slot() as usize);
        let sched = Arc::new(crate::ThreadSchedCell::new_test(id, policy));
        let core = Arc::new(ThreadCore::new(id, policy, sched, None, None, None));
        self.enqueue_task(
            QueuedThread::new(
                id,
                ActiveSchedulingState::new(policy, SchedulingEntity::new(policy, 1, 0)),
                core,
                quota_exempt,
                true,
                RqTaskMetadata::test(1),
            ),
            EnqueueReason::Wake,
            None,
        )
    }

    fn unlink_task(&mut self, id: ThreadId, deactivate: bool) -> Option<QueuedThread> {
        let class = self.membership_class(id)?;
        if class == QueueMembershipClass::DeadlineThrottled {
            let thread = self.deadline.take_throttled(id)?;
            self.unregister_membership(id);
            return Some(thread);
        }
        let was_linked_current = self.linked_current() == Some(id);
        let scheduler_class = class.scheduler_class();
        let removed = scheduler_class
            .dequeue_task(self, class, id)
            .expect("runqueue membership must identify a linked scheduling entity");
        if !was_linked_current {
            self.fixed_placement_demand = self
                .fixed_placement_demand
                .saturating_sub(fixed_placement_demand(removed.active.policy()));
        }
        self.unregister_membership(removed.id);
        self.refresh_class_pushable(removed.id, removed.active.policy(), self.linked_current());
        if deactivate {
            self.nr_running = self
                .nr_running
                .checked_sub(1)
                .expect("deactivate_task must match one runnable entity");
        }
        Some(removed)
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

    #[cfg(test)]
    pub(super) fn dequeue(&mut self, id: ThreadId) -> Option<QueuedThread> {
        self.deactivate_task(id)
    }

    /// Returns whether `id` is the RT/DL entity retained as current.
    pub(crate) fn is_linked_current(&self, id: ThreadId) -> bool {
        self.linked_current() == Some(id)
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
            QueueMembershipClass::Realtime(priority) => {
                Some(self.rt.get_mut(priority, id)?.active.entity_mut())
            }
            _ => None,
        }
    }

    pub(crate) fn capture_linked_fair_migration(
        &mut self,
        id: ThreadId,
        virtual_time: u64,
        timing_granularity_ns: u64,
    ) -> bool {
        let active = match self.membership_class(id) {
            Some(QueueMembershipClass::Deadline(key)) => {
                self.deadline.get_mut(key).map(|thread| &mut thread.active)
            }
            Some(QueueMembershipClass::Realtime(priority)) => self
                .rt
                .get_mut(priority, id)
                .map(|thread| &mut thread.active),
            _ => None,
        };
        let Some(active) = active else {
            return false;
        };
        active
            .base_entity_mut()
            .capture_fair_migration(virtual_time, timing_granularity_ns);
        true
    }

    pub(crate) fn linked_current_entity(&self, id: ThreadId) -> Option<&SchedulingEntity> {
        if self.linked_current() != Some(id) {
            return None;
        }
        match self.membership_class(id)? {
            QueueMembershipClass::Deadline(key) => self.deadline.get(key).map(QueuedThread::entity),
            QueueMembershipClass::Realtime(priority) => {
                self.rt.get(priority, id).map(QueuedThread::entity)
            }
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

    #[cfg(test)]
    pub(crate) fn debug_owns_schedule_state(&self, id: ThreadId) -> bool {
        self.queued_thread_including_current(id).is_some()
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
        let policy = thread.active.policy();
        self.enqueue_task(thread, EnqueueReason::PolicyChanged, None)?;
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_sub(fixed_placement_demand(policy));
        self.refresh_class_pushable(id, policy, Some(id));
        Ok(())
    }
}
