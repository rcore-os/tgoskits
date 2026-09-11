//! Preemption under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    pub(super) fn commit_requested_preemption_in_rq(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        mut transaction: OwnerRqTxn<'_>,
        state: RequestedPreemptionState,
    ) -> RequestedPreemptionCommit {
        let next = self.pick_owner_next_after_preemption_in_rq(
            cpu.as_mut(),
            &mut transaction,
            state.previous,
        );
        let OwnerNext {
            core: next_core,
            policy: next_policy_ref,
            urgency: next_urgency,
        } = next;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1206, next_core.as_ref().id().as_u64() as usize)
        });
        let migrated = state.migration.is_some();
        let handoff = Self::prepare_switch_handoff(
            state.previous,
            state.previous_core,
            next_core,
            next_policy_ref,
            PreviousSwitchDisposition::Live,
            state.migration,
        );
        let reason = if migrated {
            SwitchReason::Migrated
        } else {
            SwitchReason::Preempted
        };
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        self.commit_owner_switch_selection(
            cpu.as_mut(),
            transaction,
            handoff,
            !migrated && !state.dispatch.has_deferred_task_lock_work(),
        );
        let decision =
            Self::owner_switch_plan(state.previous_endpoint, next_endpoint, reason, state.now_ns);
        RequestedPreemptionCommit {
            decision,
            previous_urgency: state.previous_urgency,
            next_urgency,
            dispatch: state.dispatch,
            deadline_rq_observation,
        }
    }

    pub(super) fn finish_requested_preemption(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        commit: RequestedPreemptionCommit,
    ) -> SchedulerOutcome {
        self.finish_owner_dispatch_commit(commit.dispatch);
        self.finish_owner_selection(
            cpu.as_mut(),
            commit.decision.previous(),
            commit.decision.next(),
            commit.previous_urgency,
            commit.next_urgency,
            OwnerSchedulerDeadline::Reevaluate(commit.deadline_rq_observation),
        );
        SchedulerOutcome::Decision(commit.decision)
    }

    pub(super) fn lone_realtime_preemption_keeps_dispatch(
        &self,
        transaction: &mut OwnerRqTxn<'_>,
        current: &ThreadCore,
    ) -> bool {
        let Some(current_policy) = transaction.current().map(CurrentDispatch::schedule_policy)
        else {
            return false;
        };
        realtime_current_remains_selected(transaction, current_policy)
            && self
                .prepare_owner_rq_schedule_out(transaction, current)
                .is_some()
    }

    pub(super) fn finish_owner_no_switch(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        mut transaction: OwnerRqTxn<'_>,
        current: ThreadId,
        request_scope: SchedulerRequestScope,
        scheduler_deadline: OwnerSchedulerDeadline,
    ) -> Result<SchedulerOutcome, TaskError> {
        let runtime_overrun_work = self.sync_owner_current_dispatch_in_rq(&mut transaction);
        let request = transaction.commit_and_finish_scheduler_request();

        if let Some(core) = runtime_overrun_work {
            self.publish_deadline_overrun_work(core);
        }
        let run_queue_changed = if request.owner_work_requested()
            && self.owner_balance_work_pending(cpu.as_ref().get_ref(), current)
        {
            self.service_owner_balance(cpu.as_mut(), current)?
                .run_queue_changed()
        } else {
            false
        };
        match (run_queue_changed, scheduler_deadline) {
            (true, _) => self.program_local_timer(
                cpu.as_mut(),
                SchedulerDeadlineDerivationSource::ScheduleNoSwitch,
            )?,
            (false, OwnerSchedulerDeadline::Unchanged) => {}
            (false, OwnerSchedulerDeadline::Reevaluate(deadline_rq_observation)) => self
                .program_local_timer_from_rq_observation(
                    cpu.as_mut(),
                    deadline_rq_observation,
                    SchedulerDeadlineDerivationSource::ScheduleNoSwitch,
                )?,
        }
        Ok(
            if cpu.scheduler_request_pending(request_scope) || cpu.has_remote_work() {
                SchedulerOutcome::OwnerWorkPending
            } else {
                SchedulerOutcome::Quiescent
            },
        )
    }

    /// Services sticky scheduler work and switches only for a real preemption.
    ///
    /// `current` must be the architecture-published task identity. The owner
    /// runqueue transaction revalidates it against `rq->curr` before use.
    pub fn schedule_if_requested(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<SchedulerOutcome, TaskError> {
        self.schedule_if_requested_owner(
            cpu,
            current.runtime_core_arc(),
            OwnerRqEntry::IrqSave,
            SchedulerRequestScope::All,
        )
    }

    /// Services scheduler work while the runtime owns the IRQ-off baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn schedule_if_requested_in_scheduler_frame(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &CurrentThreadRef,
        request_scope: SchedulerRequestScope,
    ) -> Result<SchedulerOutcome, TaskError> {
        self.schedule_if_requested_owner(
            cpu,
            current.runtime_core(),
            OwnerRqEntry::SchedulerFrame,
            request_scope,
        )
    }

    pub(super) fn schedule_if_requested_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadCore,
        rq_entry: OwnerRqEntry,
        request_scope: SchedulerRequestScope,
    ) -> Result<SchedulerOutcome, TaskError> {
        let validate_owner = rq_entry.requires_owner_context_validation();
        if validate_owner {
            self.ensure_owner_cpu_context(&cpu)?;
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and switch tail are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let initial_request = remote.claim_scheduler_request(request_scope);
        self.drain_owner_work(cpu.as_mut())?;
        if validate_owner {
            self.ensure_owner_cpu_registration_online(&cpu)?;
        }
        let previous_core_hint = current;
        // Probe the rq-owned decision first. Linux's ordinary no-switch pass
        // never acquires p->pi_lock; task scheduler state is needed only after
        // this transaction proves that put_prev_task() will run.
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        transaction.adopt_scheduler_request(initial_request);
        if transaction.current().is_some() {
            let _settled = transaction.settle_current(0);
        }
        // This claim is the decision boundary: requests published by current
        // accounting participate in this pass; later sticky publications stay
        // set for the scheduler loop's final recheck.
        let mut request = transaction.merge_scheduler_request(request_scope);
        if request_scope == SchedulerRequestScope::Immediate
            && request.immediate_preempt_requested()
        {
            // Once an ordinary request enters `__schedule()`, Linux clears
            // both task flags. Claim a concurrent/lower-priority lazy request
            // as part of that same scheduling decision.
            request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        }
        if transaction.current_core_ref().map(ThreadCore::state) == Some(ThreadState::Parking) {
            // The interrupted owner still holds a generation-checked park
            // token and remains `current` / `on_cpu`. Consume this safe-point
            // doorbell so an IRQ-return `while need_resched` loop can return to
            // `commit_park`. A real preemption request is kept separately and
            // restored only if the park is cancelled.
            cpu.defer_park_preemption(request);
            transaction.commit_and_finish_scheduler_request();
            return Ok(SchedulerOutcome::ParkingDeferred);
        }
        let switch_requested = request.preemption_requested();
        let previous = transaction.current_thread();
        if transaction
            .current_core_ref()
            .is_none_or(|current| !core::ptr::eq(current, previous_core_hint))
        {
            task_runtime::fatal_invariant(0x5343_1204, cpu.owner().as_u32() as usize);
        }
        if !switch_requested
            || self.lone_realtime_preemption_keeps_dispatch(&mut transaction, previous_core_hint)
        {
            let deadline_rq_observation =
                transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
            return self.finish_owner_no_switch(
                cpu.as_mut(),
                transaction,
                previous_core_hint.id(),
                request_scope,
                OwnerSchedulerDeadline::Reevaluate(deadline_rq_observation),
            );
        }
        if let Some(schedule_out) =
            self.prepare_owner_rq_schedule_out(&transaction, previous_core_hint)
        {
            let now_ns = transaction.clock().wall().as_nanos();
            let dispatch_commit = self.sync_owner_settled_current_dispatch_in_rq(&mut transaction);
            let OwnerRqScheduledOut {
                core: previous_core,
                endpoint: previous_endpoint,
                fifo: _,
                urgency: previous_urgency,
                realtime_yield_head: _,
            } = self.schedule_out_owner_rq_owned(
                &mut transaction,
                schedule_out,
                EnqueueReason::Preempted,
            );
            let commit = self.commit_requested_preemption_in_rq(
                cpu.as_mut(),
                transaction,
                RequestedPreemptionState {
                    previous,
                    previous_core: Some(previous_core),
                    previous_endpoint: Some(previous_endpoint),
                    previous_urgency: Some(previous_urgency),
                    dispatch: dispatch_commit,
                    migration: None,
                    now_ns,
                },
            );
            return Ok(self.finish_requested_preemption(cpu.as_mut(), commit));
        }
        // Preserve the merged preemption decision while releasing rq.
        // Publications in this gap leave their sticky bits set and are merged
        // by the second transaction instead of being lost.
        transaction.commit();

        // A real switch follows the established p->pi_lock -> rq order. The
        // second rq pass resamples its clock and revalidates current rather
        // than carrying a stale snapshot across the unlocked interval.
        // SAFETY: propagated from the selected entry contract.
        let mut previous_sched = unsafe { rq_entry.lock_thread_sched(previous_core_hint.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        transaction.adopt_scheduler_request(request);
        if transaction.current().is_some() {
            let _settled = transaction.settle_current(0);
        }
        let request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        if !request.preemption_requested() {
            task_runtime::fatal_invariant(0x5343_120a, cpu.owner().as_u32() as usize);
        }
        let now_ns = transaction.clock().wall().as_nanos();
        let previous = transaction.current_thread();
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        if previous_core
            .as_ref()
            .is_none_or(|core| !core::ptr::eq(core.as_ref(), previous_core_hint))
        {
            task_runtime::fatal_invariant(0x5343_1204, cpu.owner().as_u32() as usize);
        }
        let dispatch_commit = self.sync_owner_settled_current_dispatch_in_rq(&mut transaction);
        let previous_urgency = transaction.current_scheduling_urgency();
        let mut migration = None;
        if let Some(core) = previous_core.as_ref() {
            let schedule_out = self.schedule_out_owner_running_in_rq(
                cpu.as_mut(),
                &mut transaction,
                Arc::clone(core),
                &mut previous_sched,
                now_ns,
                EnqueueReason::Preempted,
            );
            migration = schedule_out.migration;
        }
        let commit = self.commit_requested_preemption_in_rq(
            cpu.as_mut(),
            transaction,
            RequestedPreemptionState {
                previous,
                previous_core: previous_core.map(PreviousSwitchOwnership::retained),
                previous_endpoint,
                previous_urgency,
                dispatch: dispatch_commit,
                migration,
                now_ns,
            },
        );
        drop(previous_sched);
        Ok(self.finish_requested_preemption(cpu.as_mut(), commit))
    }
}
