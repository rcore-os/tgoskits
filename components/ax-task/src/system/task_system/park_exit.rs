//! Park, current-thread exit, and physical switch-tail completion.

use super::*;

impl TaskSystem {
    /// Publishes `PARKING` after consuming a wake-before-park notification.
    pub fn prepare_park(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<ParkPrepare, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
        if core.take_park_notification() {
            return Ok(ParkPrepare::Notified);
        }
        let generation = core.next_park_generation()?;
        core.sched().lock().transition(core, ThreadState::Parking)?;
        Ok(ParkPrepare::Prepared(ParkTicket::new(
            core.id(),
            generation,
        )))
    }

    /// Rechecks a prepared park and either cancels it or commits schedule-out.
    ///
    /// `now_ns` is the single monotonic snapshot for this owner transition.
    /// The caller must sample it after acquiring scheduler ownership.
    pub fn commit_park(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
        now_ns: u64,
    ) -> Result<ParkCommit, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if token.is_resolved() {
            return Err(TaskError::StaleThreadId);
        }
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.ensure_owner_cpu_online(&cpu)?;
        if cpu.current() != Some(token.thread()) {
            return Err(TaskError::StaleThreadId);
        }
        let previous_core = cpu
            .current_core()
            .cloned()
            .ok_or(TaskError::NoRunnableThread)?;
        let generation = previous_core.park_generation();
        if generation != token.generation() {
            return Err(TaskError::StaleThreadId);
        }
        let notified = previous_core.take_park_notification();
        if notified {
            previous_core
                .sched()
                .lock()
                .transition(&previous_core, ThreadState::Running)?;
            cpu.finish_park_preemption(true);
            token.mark_resolved();
            return Ok(ParkCommit::Notified);
        }
        cpu.as_mut().scheduler_enter();
        cpu.finish_park_preemption(false);
        self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
        {
            let mut sched = previous_core.sched().lock();
            let timing_granularity_ns = self.config.timing_granularity_ns();
            if let Some(fair) = sched.policy.effective_entity.fair() {
                let virtual_time = cpu
                    .dispatch_state()
                    .run_queue
                    .virtual_time_for_mode(fair.mode());
                sched
                    .policy
                    .effective_entity
                    .capture_fair_sleep_lag(virtual_time, timing_granularity_ns);
            }
            if !sched.is_pi_boosted()
                && let Some(fair) = sched.policy.base_entity.fair()
            {
                let virtual_time = cpu
                    .dispatch_state()
                    .run_queue
                    .virtual_time_for_mode(fair.mode());
                sched
                    .policy
                    .base_entity
                    .capture_fair_sleep_lag(virtual_time, timing_granularity_ns);
            }
            sched.transition(&previous_core, ThreadState::Blocked)?;
            sched.placement.set_running_cpu(None)?;
        }
        Self::mark_owner_deadline_non_contending(&previous_core, cpu.as_mut(), now_ns)?;
        cpu.as_mut().clear_current();
        let next = self.pick_owner_next(cpu.as_mut(), now_ns, Some(token.thread()))?;
        if next.outgoing_migration_target.is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        let next_core = next.core;
        Self::stage_switch_handoff(
            cpu.as_mut(),
            Some(token.thread()),
            Some(Arc::clone(&previous_core)),
            next_core.id(),
            None,
        )?;
        let decision =
            Self::owner_switch_plan(Some(&previous_core), &next_core, SwitchReason::Blocked);
        let decision = self.finish_owner_selection(cpu, decision, now_ns);
        token.mark_resolved();
        Ok(ParkCommit::Blocked(decision))
    }

    /// Cancels a prepared park because an independent grant won the race.
    pub fn cancel_park(
        &self,
        cpu: Pin<&mut CpuLocal>,
        token: &mut ParkTicket,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if token.is_resolved() {
            return Err(TaskError::StaleThreadId);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        if cpu.current() != Some(token.thread()) {
            return Err(TaskError::StaleThreadId);
        }
        let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
        if core.park_generation() != token.generation() {
            return Err(TaskError::StaleThreadId);
        }
        core.sched().lock().transition(core, ThreadState::Running)?;
        cpu.finish_park_preemption(true);
        token.mark_resolved();
        Ok(())
    }

    /// Parks the current thread and selects its replacement.
    ///
    /// `now_ns` is the single monotonic snapshot for the complete
    /// prepare-to-commit transaction.
    pub fn block_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        match self.prepare_park(cpu.as_mut())? {
            ParkPrepare::Prepared(mut ticket) => {
                match self.commit_park(cpu.as_mut(), &mut ticket, now_ns)? {
                    ParkCommit::Blocked(decision) => Ok(decision),
                    ParkCommit::Notified => {
                        let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
                        Ok(Self::owner_switch_plan(
                            Some(core),
                            core,
                            SwitchReason::Blocked,
                        ))
                    }
                }
            }
            ParkPrepare::Notified => {
                let core = cpu.current_core().ok_or(TaskError::NoRunnableThread)?;
                Ok(Self::owner_switch_plan(
                    Some(core),
                    core,
                    SwitchReason::Blocked,
                ))
            }
        }
    }

    /// Validates all fallible current-thread exit prerequisites without
    /// publishing the thread as exited.
    pub fn prepare_current_exit(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ThreadId, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let current = cpu.current().ok_or(TaskError::NoRunnableThread)?;
        if cpu.idle() == Some(current) {
            return Err(TaskError::InvalidConfiguration);
        }
        let record = state.thread_record(current)?;
        let sched = record.sched.lock();
        let lifecycle = sched.lifecycle.state();
        if lifecycle != ThreadState::Running {
            return Err(TaskError::InvalidTransition {
                from: lifecycle,
                to: ThreadState::Exited,
            });
        }
        if record.blocked_on.is_some()
            || record.pi_waiter_head.is_some()
            || sched.pi.blocked_waiters != 0
        {
            return Err(TaskError::InvalidPiState);
        }
        if sched.placement.running_cpu() != Some(cpu.owner())
            || sched.placement.on_cpu() != Some(cpu.owner())
        {
            return Err(TaskError::ThreadBusy);
        }
        if record.resources.context().is_none() {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        Ok(current)
    }

    /// Commits current-thread exit and selects a replacement.
    ///
    /// `now_ns` is the single monotonic snapshot shared by dispatch
    /// accounting, successor selection, tracing, and the runtime switch.
    pub fn exit_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.commit_current_exit_after_owner_drain(cpu, now_ns)
    }

    /// Commits the non-returning half of current exit after owner work drained.
    ///
    /// The scheduler activity gate closes the intentional drain-to-commit
    /// window against a newly publishing remote policy or affinity update. A
    /// message that won before the gate remains an in-flight late delivery and
    /// pins registry resources until its owner drains it as an exited no-op.
    pub(super) fn commit_current_exit_after_owner_drain(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        let (decision, exited_core) = {
            let mut state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let previous = cpu.current().ok_or(TaskError::NoRunnableThread)?;
            let previous_core = cpu.current_core().cloned();
            if state.thread_record(previous)?.has_live_pi_edges() {
                return Err(TaskError::InvalidPiState);
            }
            cpu.as_mut().scheduler_enter();
            self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
            let previous_core = previous_core.ok_or(TaskError::NoRunnableThread)?;
            Self::detach_owner_deadline_bandwidth(&previous_core, cpu.as_mut())?;
            let _exit = previous_core
                .try_scheduler_exit()
                .ok_or(TaskError::ThreadBusy)?;
            {
                let mut sched = previous_core.sched().lock();
                sched.placement.set_migration_target(None)?;
                sched.transition(&previous_core, ThreadState::Exited)?;
                sched.placement.mark_exited_awaiting_tail(cpu.owner())?;
                let record = state.thread_record_mut(previous)?;
                record.callbacks.prepare_exit(record.extension.is_some())?;
            }
            state.queue_exited_thread(previous);
            state.release_deadline_reservation_on_exit(previous)?;
            cpu.as_mut().clear_current();
            let next = self.pick_owner_next(cpu.as_mut(), now_ns, Some(previous))?;
            if next.outgoing_migration_target.is_some() {
                return Err(TaskError::InvalidConfiguration);
            }
            let next_core = next.core;
            Self::stage_switch_handoff(
                cpu.as_mut(),
                Some(previous),
                Some(Arc::clone(&previous_core)),
                next_core.id(),
                None,
            )?;
            (
                Self::owner_switch_plan(Some(&previous_core), &next_core, SwitchReason::Exited),
                Arc::clone(&previous_core),
            )
        };
        exited_core.notify_affinity_waiters();
        Ok(self.finish_owner_selection(cpu, decision, now_ns))
    }

    /// Completes the physical switch-out handoff in the newly active context.
    ///
    /// This second phase clears `on_cpu` only after architecture execution has
    /// left the previous stack. Deferred migration publication and exit hooks
    /// therefore cannot make a context runnable or reapable too early.
    pub fn complete_context_switch(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let Some(initial_handoff) = cpu.as_ref().get_ref().switch_handoff().cloned() else {
            return Ok(());
        };
        let owner = cpu.owner();
        {
            let bandwidth = cpu.as_ref().get_ref().deadline_bandwidth();
            let sched = initial_handoff.previous.sched().lock();
            self.validate_switch_handoff_state(owner, bandwidth, &initial_handoff, &sched)?;
        }

        if !initial_handoff.runtime_tail_finished {
            ensure_runtime_success(task_runtime::finish_context_switch_tail())?;
            if cpu
                .as_mut()
                .finish_switch_runtime_tail(
                    initial_handoff.previous.id(),
                    initial_handoff.migration_target,
                )
                .is_err()
            {
                task_runtime::fatal_invariant(
                    0x5357_0001,
                    initial_handoff.previous.id().as_u64() as usize,
                );
            }
        }

        let handoff = cpu
            .as_ref()
            .get_ref()
            .switch_handoff()
            .cloned()
            .ok_or(TaskError::InvalidConfiguration)?;
        let previous = handoff.previous.id();
        let (migration_target, previous_exited) = {
            let bandwidth = cpu.as_ref().get_ref().deadline_bandwidth();
            let mut sched = handoff.previous.sched().lock();
            let (migration_target, previous_exited) =
                self.validate_switch_handoff_state(owner, bandwidth, &handoff, &sched)?;
            if migration_target.is_some() && sched.deadline.bandwidth_cpu.is_some() {
                cpu.as_mut().remove_deadline_bandwidth(
                    sched.deadline.bandwidth_scaled,
                    sched.deadline.activity != DeadlineActivity::Inactive,
                )?;
                sched.deadline.bandwidth_cpu = None;
                cpu.as_mut().unregister_deadline_member(&handoff.previous);
            }
            sched.placement.set_on_cpu(None)?;
            if let Some(target) = migration_target {
                handoff.previous.set_target_cpu(target);
            }
            (migration_target, previous_exited)
        };
        if let Some(target) = migration_target
            && self
                .publish_owner_migration(&handoff.previous, target, owner, target)
                .is_err()
        {
            // Target loss is normally recovered through the still-running
            // source inbox. Failure here means even that owner can no longer
            // accept the post-switch placement transaction.
            task_runtime::fatal_invariant(0x5357_0002, target.as_u32() as usize);
        }
        let consumed = cpu.as_mut().take_switch_handoff().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5357_0003, previous.as_u64() as usize)
        });
        if consumed.previous.id() != previous
            || consumed.migration_target != handoff.migration_target
            || !consumed.runtime_tail_finished
        {
            task_runtime::fatal_invariant(0x5357_0004, previous.as_u64() as usize);
        }
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        if previous_exited {
            self.task_work.publish();
        }
        Ok(())
    }

    fn validate_switch_handoff_state(
        &self,
        owner: CpuId,
        bandwidth: DeadlineBandwidthSnapshot,
        handoff: &crate::system::cpu::SwitchHandoff,
        sched: &ThreadSchedState,
    ) -> Result<(Option<CpuId>, bool), TaskError> {
        if sched.placement.on_cpu() != Some(owner) {
            return Err(TaskError::InvalidConfiguration);
        }
        let migration_target = match handoff.migration_target {
            Some(_) => {
                let target = sched
                    .placement
                    .migration_target()
                    .ok_or(TaskError::InvalidConfiguration)?;
                if sched.lifecycle.state() != ThreadState::Ready
                    || sched.placement.queued_cpu().is_some()
                    || sched.placement.running_cpu().is_some()
                {
                    return Err(TaskError::InvalidConfiguration);
                }
                if self.cpu_remote(target).is_none() {
                    return Err(TaskError::CpuOffline(target.as_u32()));
                }
                if let Some(assigned) = sched.deadline.bandwidth_cpu {
                    if assigned != owner {
                        return Err(TaskError::CpuOwnerMismatch {
                            expected: assigned.as_u32(),
                            actual: owner.as_u32(),
                        });
                    }
                    if bandwidth.this_bw_scaled() < sched.deadline.bandwidth_scaled
                        || (sched.deadline.activity != DeadlineActivity::Inactive
                            && bandwidth.running_bw_scaled() < sched.deadline.bandwidth_scaled)
                    {
                        return Err(TaskError::InvalidConfiguration);
                    }
                }
                Some(target)
            }
            None => None,
        };
        Ok((
            migration_target,
            sched.lifecycle.state() == ThreadState::Exited,
        ))
    }
}
