use super::*;

impl RunQueue {
    /// Makes the retained RT/DL current queued again without transferring its
    /// intrusive node or membership identity.
    pub(crate) fn put_prev_task(&mut self, id: ThreadId) -> Result<SchedulingEntity, TaskError> {
        if self.linked_current() != Some(id) {
            return Err(TaskError::NotReady);
        }
        let class = self.membership_class(id).ok_or(TaskError::NotReady)?;
        let policy = self
            .queued_thread_including_current(id)
            .ok_or(TaskError::NotReady)?
            .policy;
        let entity = SchedulerClass::for_policy(policy).put_prev_task(self, class, id)?;
        self.refresh_class_pushable(id, None);
        self.mark_publication_dirty();
        Ok(entity)
    }

    /// Runs the fixed-priority RT put-prev hook without cloning its unchanged
    /// scheduling entity for a lock-free publication that is not needed.
    #[inline(always)]
    pub(crate) fn yield_realtime_current(&mut self, id: ThreadId) -> Result<(), TaskError> {
        let Some(QueueMembershipClass::Realtime(key)) = self.membership_class(id) else {
            return Err(TaskError::InvalidConfiguration);
        };
        if !self.rt.yield_current(key) {
            return Err(TaskError::NotReady);
        }
        Ok(())
    }

    /// Linux `put_prev_task_rt()`: updates current accounting and pushable
    /// state without implementing `yield_task_rt()` as an enqueue reason.
    #[inline(always)]
    pub(crate) fn put_prev_realtime_task(&mut self, id: ThreadId, migration_capable: bool) {
        // Common runtime accounting already performed Linux's
        // `update_curr_rt()`. A fixed-affinity current is never in the
        // pushable set, so `put_prev_task_rt()` has no remaining work. The
        // caller's `LinkedRealtime` selection retains the rq membership proof
        // for this complete transaction.
        if migration_capable {
            self.refresh_class_pushable(id, None);
        }
    }

    /// Moves one queued RT wakee ahead of its equal-priority current.
    pub(crate) fn requeue_realtime_wakee_head(&mut self, id: ThreadId) -> bool {
        let Some(QueueMembershipClass::Realtime(key)) = self.membership_class(id) else {
            return false;
        };
        self.rt.requeue_head(key)
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
        let class = self.membership_class(id)?;
        let is_fair = matches!(class, QueueMembershipClass::Fair);
        if is_fair {
            self.update_fair_virtual_time(current_fair);
        }
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
        self.refresh_class_pushable(thread.id, self.linked_current());
        if is_fair {
            self.update_fair_virtual_time(current_fair);
        }
        self.mark_publication_dirty();
        Some(thread)
    }

    #[inline(always)]
    pub(crate) fn pick_next_task(
        &mut self,
        rt_eligibility: RtEligibility,
        skip_delayed: bool,
        protected_fair_current: Option<ThreadId>,
    ) -> Option<PickTaskResult> {
        for class in SchedulerClass::PICK_ORDER {
            if let Some(picked) =
                class.pick_task(self, rt_eligibility, skip_delayed, protected_fair_current)
            {
                return Some(picked);
            }
        }
        None
    }

    /// Selects the RT class head after the caller has proved that no higher
    /// scheduler class is runnable and the RT runqueue is not throttled.
    #[inline(always)]
    pub(crate) fn pick_realtime_task(&self) -> Option<LinkedRqTaskRef> {
        self.rt.select()
    }

    /// Linux `set_next_task()`: commits the rq-owned class pick as current.
    ///
    /// Class selection itself is the ownership proof: every runnable entity
    /// can enter a class queue only through the owner-rq enqueue and migration
    /// transactions. Like Linux, the hot pick path therefore does not reopen
    /// task lifecycle, affinity, or placement state before publishing
    /// `on_cpu`; `SchedulerPlacement::set_next_task` checks the packed
    /// `task_cpu`/`on_rq` carrier at that final publication boundary.
    #[inline(always)]
    pub(crate) fn set_next_task(&mut self, picked: &PickedThread) {
        SchedulerClass::for_policy(picked.policy()).set_next_task(self, picked);
        if picked.metadata().affinity.is_migration_capable() {
            self.refresh_class_pushable(picked.id(), Some(picked.id()));
        }
    }
}
