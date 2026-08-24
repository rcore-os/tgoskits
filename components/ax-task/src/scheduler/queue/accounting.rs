use super::*;

impl RunQueue {
    /// Charges `rq->curr` and its class-owned entity in one rq transaction.
    ///
    /// The common dispatch token and RT/DL active nodes are disjoint fields
    /// of the same rq. Keeping this operation here prevents callers from
    /// temporarily clearing `rq->curr` merely to obtain two mutable borrows.
    pub(crate) fn charge_current(
        &mut self,
        runtime_ns: u64,
        now_ns: u64,
        inactive_bw_scaled: u64,
        extra_bw_scaled: u64,
        max_bw_scaled: u64,
        reclaimed_ns: u64,
    ) -> Result<(DispatchCharge, SchedulePolicy, SchedulingEntity, bool), TaskError> {
        let current = self.current.as_ref().ok_or(TaskError::NoRunnableThread)?;
        let id = current.thread();
        let policy = current.schedule_policy();
        let rt_quota_exempt = current.rt_quota_exempt();
        let membership = self.membership_class(id);
        let current_entity = match membership {
            Some(QueueMembershipClass::Deadline(key)) => self
                .deadline
                .get(key)
                .map(QueuedThread::entity_snapshot)
                .ok_or(TaskError::InvalidConfiguration)?,
            Some(QueueMembershipClass::Realtime(key)) => self
                .rt
                .get(key)
                .map(QueuedThread::entity_snapshot)
                .ok_or(TaskError::InvalidConfiguration)?,
            _ => current
                .owned_scheduling_entity_ref()
                .cloned()
                .ok_or(TaskError::InvalidConfiguration)?,
        };
        let dispatch = self.current.as_mut().ok_or(TaskError::NoRunnableThread)?;
        let grub_reclaimed_ns = dispatch.grub_reclaimed_ns(
            &current_entity,
            runtime_ns,
            inactive_bw_scaled,
            extra_bw_scaled,
            max_bw_scaled,
        );
        let reclaimed_ns = reclaimed_ns.saturating_add(grub_reclaimed_ns);
        let charge = match membership {
            Some(QueueMembershipClass::Deadline(key)) => {
                let entity = &mut self
                    .deadline
                    .get_mut(key)
                    .ok_or(TaskError::InvalidConfiguration)?
                    .active
                    .entity_mut();
                dispatch.charge_linked(entity, runtime_ns, now_ns, reclaimed_ns)
            }
            Some(QueueMembershipClass::Realtime(key)) => {
                let entity = &mut self
                    .rt
                    .get_mut(key)
                    .ok_or(TaskError::InvalidConfiguration)?
                    .active
                    .entity_mut();
                dispatch.charge_linked(entity, runtime_ns, now_ns, reclaimed_ns)
            }
            _ => dispatch.charge(runtime_ns, now_ns, reclaimed_ns),
        };
        let charged_entity = match membership {
            Some(QueueMembershipClass::Deadline(key)) => self
                .deadline
                .get(key)
                .map(QueuedThread::entity_snapshot)
                .ok_or(TaskError::InvalidConfiguration)?,
            Some(QueueMembershipClass::Realtime(key)) => self
                .rt
                .get(key)
                .map(QueuedThread::entity_snapshot)
                .ok_or(TaskError::InvalidConfiguration)?,
            _ => self
                .current
                .as_ref()
                .and_then(CurrentDispatch::owned_scheduling_entity_ref)
                .cloned()
                .ok_or(TaskError::InvalidConfiguration)?,
        };
        Ok((charge, policy, charged_entity, rt_quota_exempt))
    }

    /// Reserves every class index before a thread becomes externally visible.
    /// Scheduler fast paths treat missing capacity as an invariant violation
    /// instead of allocating under the irqsave rq lock.
    pub(crate) fn prepare_thread_slot(&mut self, slot: usize) {
        if self.membership.len() <= slot {
            self.membership.resize(slot.saturating_add(1), None);
        }
        self.deadline.prepare_thread_slot(slot);
        self.fair.prepare_thread_slot(slot);
        self.idle_fair.prepare_thread_slot(slot);
    }

    pub(crate) const fn nr_running(&self) -> usize {
        self.nr_running
    }

    pub(crate) fn nr_queued(&self) -> usize {
        let current_runnable = self
            .current
            .as_ref()
            .is_some_and(|current| !current.is_dedicated_idle());
        self.nr_running
            .checked_sub(usize::from(current_runnable))
            .expect("rq->curr runnable state must be included in rq->nr_running")
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(super) fn len(&self) -> usize {
        self.nr_queued()
    }

    /// Deactivates a Fair/stop current whose entity is intentionally outside
    /// every active class structure while it runs.
    pub(crate) fn deactivate_unlinked_current(&mut self, id: ThreadId) {
        assert!(
            !self.contains(id),
            "an rq-linked current must be deactivated through its class"
        );
        self.nr_running = self
            .nr_running
            .checked_sub(1)
            .expect("current deactivation must match one runnable entity");
    }

    pub(crate) fn fair_demand(&self) -> u64 {
        self.fair
            .total_weight()
            .saturating_add(self.idle_fair.total_weight())
    }

    pub(crate) fn placement_demand(&self) -> u64 {
        self.fair_demand()
            .saturating_add(self.fixed_placement_demand)
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) const fn virtual_time(&self) -> u64 {
        self.fair.virtual_time()
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(crate) fn set_virtual_time_for_test(&mut self, virtual_time: u64) {
        self.fair.set_virtual_time_for_test(virtual_time);
    }

    pub(crate) const fn virtual_time_for_mode(&self, mode: FairMode) -> u64 {
        if matches!(mode, FairMode::Idle) {
            self.idle_fair.virtual_time()
        } else {
            self.fair.virtual_time()
        }
    }

    /// Updates each fair class's authoritative weighted-average virtual time.
    ///
    /// `current` is supplied because the running entity is temporarily absent
    /// from the owner runqueue. Like Linux `avg_vruntime()`, insertion and
    /// removal may move this average in either direction; saved `vlag` protects
    /// entities from those membership changes.
    pub(crate) fn update_fair_virtual_time(&mut self, current: Option<FairEntity>) {
        let normal_current = current.filter(|entity| entity.mode() != FairMode::Idle);
        let idle_current = current.filter(|entity| entity.mode() == FairMode::Idle);
        self.fair.update_virtual_time(normal_current);
        self.idle_fair.update_virtual_time(idle_current);
    }

    pub(crate) fn has_rt(&self) -> bool {
        self.rt.has_any_rt()
    }

    pub(crate) fn has_exempt_rt(&self) -> bool {
        self.rt.has_exempt_rt()
    }

    pub(crate) fn highest_rt_priority(&self) -> Option<u8> {
        self.rt.highest_rt_priority()
    }

    pub(crate) fn rt_count_at_priority(&self, priority: u8) -> usize {
        self.rt.count_at_priority(priority)
    }

    pub(crate) fn has_fair(&self) -> bool {
        !self.fair.is_empty()
    }

    pub(crate) fn has_idle_fair(&self) -> bool {
        !self.idle_fair.is_empty()
    }

    pub(crate) fn min_fair_service_request_ns(&self, mode: FairMode) -> Option<u64> {
        if mode == FairMode::Idle {
            self.idle_fair.min_service_request_ns()
        } else {
            self.fair.min_service_request_ns()
        }
    }

    pub(crate) fn fair_wakee_is_selected(
        &self,
        wakee: ThreadId,
        mode: FairMode,
        virtual_time: u64,
    ) -> bool {
        let queue = if mode == FairMode::Idle {
            &self.idle_fair
        } else {
            &self.fair
        };
        queue.earliest_eligible(virtual_time) == Some(wakee)
    }

    pub(crate) fn earliest_deadline_ns(&self) -> Option<u64> {
        self.deadline.earliest_deadline_ns()
    }

    pub(crate) fn deadline_members_are_empty(&self) -> bool {
        self.deadline.members_are_empty()
    }

    pub(crate) fn deadline_member(&self, thread: ThreadId) -> Option<Arc<ThreadCore>> {
        self.deadline.member(thread)
    }

    pub(crate) fn register_deadline_member(&mut self, core: &Arc<ThreadCore>) -> bool {
        self.deadline.register_member(core)
    }

    pub(crate) fn unregister_deadline_member(&mut self, core: &Arc<ThreadCore>) {
        self.deadline.unregister_member(core);
    }

    pub(crate) fn add_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.deadline.add_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn remove_deadline_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.deadline.remove_bandwidth(utilization_scaled, active);
    }

    pub(crate) fn activate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.deadline.activate_bandwidth(utilization_scaled);
    }

    pub(crate) fn deactivate_deadline_bandwidth(&mut self, utilization_scaled: u64) {
        self.deadline.deactivate_bandwidth(utilization_scaled);
    }

    pub(crate) const fn deadline_bandwidth(&self) -> crate::DeadlineBandwidthSnapshot {
        self.deadline.bandwidth()
    }

    pub(crate) fn has_pushable_deadline(&self) -> bool {
        self.deadline.has_pushable()
    }

    pub(crate) fn has_pushable_realtime(&self) -> bool {
        self.rt.has_pushable()
    }

    pub(crate) fn has_pushable_fair(&self) -> bool {
        self.fair.has_migratable()
    }

    pub(super) fn refresh_class_pushable(&mut self, thread: ThreadId, current: Option<ThreadId>) {
        match self.membership_class(thread) {
            Some(QueueMembershipClass::Deadline(_)) => {
                self.deadline.refresh_pushable(thread, current)
            }
            Some(QueueMembershipClass::Realtime(key)) => self.rt.refresh_pushable(key, current),
            Some(
                QueueMembershipClass::Stop
                | QueueMembershipClass::DeadlineThrottled
                | QueueMembershipClass::Fair
                | QueueMembershipClass::IdleFair,
            )
            | None => {}
        }
    }
}
