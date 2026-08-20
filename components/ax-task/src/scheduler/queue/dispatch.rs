use super::*;

impl RunQueue {
    /// Restores a class pick whose owner-rq validation did not reach set-next.
    pub(crate) fn rollback_pick(&mut self, picked: PickedThread) {
        match picked {
            PickedThread::Linked(_) => {}
            PickedThread::Owned(mut thread) => {
                thread.active.entity_mut().cancel_fair_migration();
                SchedulerClass::for_policy(thread.active.policy()).rollback_pick(self, thread);
            }
        }
    }

    /// Makes the retained RT/DL current queued again without transferring its
    /// intrusive node or membership identity.
    pub(crate) fn put_prev_task(
        &mut self,
        id: ThreadId,
        reason: EnqueueReason,
    ) -> Result<SchedulingEntity, TaskError> {
        if self.linked_current() != Some(id) {
            return Err(TaskError::NotReady);
        }
        let class = self.membership_class(id).ok_or(TaskError::NotReady)?;
        let policy = self
            .queued_thread_including_current(id)
            .ok_or(TaskError::NotReady)?
            .policy;
        let entity = SchedulerClass::for_policy(policy).put_prev_task(self, class, id, reason)?;
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_add(fixed_placement_demand(policy));
        self.refresh_class_pushable(id, policy, None);
        Ok(entity)
    }

    pub(crate) fn detach_for_transfer(
        &mut self,
        id: ThreadId,
        current_fair: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> Option<QueuedThread> {
        if self.linked_current() == Some(id) {
            return None;
        }
        self.update_fair_virtual_time(current_fair);
        let class = self.membership_class(id)?;
        if class == QueueMembershipClass::DeadlineThrottled {
            // Linux keeps a throttled DL task at
            // `TASK_ON_RQ_QUEUED`, but it is absent from both the DL rb-tree
            // and `rq->nr_running`. Migration moves that queued ownership
            // without applying the ordinary runnable accounting below. The
            // destination preserves the same throttled membership until its
            // hard CBS timer replenishes it.
            let thread = self.deadline.take_throttled(id)?;
            self.unregister_membership(id);
            return Some(thread);
        }
        let thread = SchedulerClass::for_policy(self.queued_thread_including_current(id)?.policy())
            .migrate_task_rq(self, class, id, timing_granularity_ns)?;
        self.nr_running = self
            .nr_running
            .checked_sub(1)
            .expect("migration must detach one runnable entity");
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_sub(fixed_placement_demand(thread.active.policy()));
        self.unregister_membership(thread.id);
        self.refresh_class_pushable(thread.id, thread.active.policy(), self.linked_current());
        self.update_fair_virtual_time(current_fair);
        Some(thread)
    }

    pub(crate) fn pick_next_task(&mut self, rt_eligibility: RtEligibility) -> Option<PickedThread> {
        SchedulerClass::PICK_ORDER
            .into_iter()
            .find_map(|class| class.pick_task(self, rt_eligibility))
    }

    /// Linux `set_next_task()`: commits one class pick as current.
    pub(crate) fn set_next_task(&mut self, picked: &PickedThread) {
        self.fixed_placement_demand = self
            .fixed_placement_demand
            .saturating_sub(fixed_placement_demand(picked.policy()));
        SchedulerClass::for_policy(picked.policy()).set_next_task(self, picked);
        self.refresh_class_pushable(picked.id(), picked.policy(), Some(picked.id()));
    }
}
