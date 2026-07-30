//! Owner scheduling entry points, runtime charging, and load balancing requests.

use super::*;

impl TaskSystem {
    /// Requests one owner-mediated pull from the busiest remote CPU.
    ///
    /// The target never locks or mutates the source runqueue. Its pinned request
    /// node is published to the source migration inbox and the source owner
    /// selects and hands off one affinity-compatible thread at a safe point.
    pub fn request_idle_pull(&self, cpu: Pin<&CpuLocal>) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if task_runtime::in_hard_irq() {
            return Ok(false);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        if cpu.current() != cpu.idle()
            || cpu.has_remote_work()
            || cpu.try_runnable_summary() != Some(0)
        {
            return Ok(false);
        }
        let target_remote = cpu.remote();
        let reservation = match target_remote.begin_idle_pull() {
            IdlePullReservation::Started(reservation) => reservation,
            IdlePullReservation::AlreadyPending => return Ok(true),
            IdlePullReservation::Busy => return Ok(false),
        };
        if cpu.current() != cpu.idle()
            || cpu.has_remote_work()
            || cpu.try_runnable_summary() != Some(0)
        {
            target_remote.cancel_idle_pull(reservation);
            return Ok(false);
        }
        let now_ns = task_runtime::monotonic_ns();
        let target = cpu.owner();
        let source = self
            .cpu_remotes
            .iter()
            .enumerate()
            .filter(|(index, remote)| remote.is_online() && CpuId::new(*index as u32) != target)
            .filter_map(|(index, local)| {
                let source = CpuId::new(index as u32);
                let summary = local.try_load_summary()?;
                let key = summary.pushable_key()?;
                let class = summary.pushable_class()?;
                if !summary.is_overloaded()
                    || (class == SchedulingClass::Fair && !local.fair_balance_due(now_ns))
                {
                    return None;
                }
                Some((class, key, summary.runnable_count(), source))
            })
            .min_by_key(|(class, key, load, source)| {
                let cross_cpu_urgency =
                    matches!(class, SchedulingClass::Deadline | SchedulingClass::Realtime)
                        .then_some(*key);
                (
                    *class as u8,
                    cross_cpu_urgency,
                    core::cmp::Reverse(*load),
                    source.as_u32(),
                )
            });
        let Some((_, _, _, source)) = source else {
            target_remote.cancel_idle_pull(reservation);
            return Ok(false);
        };
        let Some(source_local) = self.cpu_remote(source) else {
            target_remote.cancel_idle_pull(reservation);
            return Err(TaskError::CpuOffline(source.as_u32()));
        };
        let message = InboxMessage::balance_request(source, target, reservation);
        let result = source_local.publish_migration(cpu.balance_request_node(), message);
        match result {
            PublishResult::Published => Ok(true),
            PublishResult::AlreadyPending => {
                target_remote.cancel_idle_pull(reservation);
                Ok(true)
            }
            PublishResult::WrongKind => {
                target_remote.cancel_idle_pull(reservation);
                Ok(false)
            }
        }
    }

    /// Pushes one queued thread from an overloaded owner to the least loaded CPU.
    ///
    /// Selection and dequeue happen only on `cpu`; the target receives an
    /// intrusive handoff and enqueues it in its own safe-point drain.
    pub fn push_overloaded(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<Option<ThreadId>, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if task_runtime::in_hard_irq() {
            return Ok(None);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        self.push_overloaded_from_published_summary(cpu)
    }

    /// Pushes from the coherent owner snapshot published by the immediately
    /// preceding runqueue transaction.
    ///
    /// Scheduler selection publishes after installing its next dispatch, so
    /// its common tail can reuse that snapshot just as Linux keeps balancing
    /// decisions under one owner-rq transaction. Callers must not mutate the
    /// local runqueue or current dispatch between publication and this call.
    pub(super) fn push_overloaded_from_published_summary(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<Option<ThreadId>, TaskError> {
        let source = cpu.owner();
        let Some(source_summary) = cpu.try_load_summary() else {
            return Ok(None);
        };
        if !source_summary.is_overloaded()
            || !matches!(
                source_summary.pushable_class(),
                Some(SchedulingClass::Deadline | SchedulingClass::Realtime)
            )
        {
            return Ok(None);
        }
        let target = self
            .cpu_remotes
            .iter()
            .enumerate()
            .filter(|(index, remote)| remote.is_online() && CpuId::new(*index as u32) != source)
            .filter_map(|(index, remote)| {
                let target = CpuId::new(index as u32);
                let target_summary = remote.try_load_summary()?;
                if target_summary.runnable_count() >= source_summary.runnable_count() {
                    return None;
                }
                let candidate = self.select_owner_balance_candidate(
                    cpu.as_ref().get_ref(),
                    Some(target),
                    0,
                    BalanceReason::RtDeadlinePush,
                )?;
                let key = candidate.balance_key();
                if target_summary
                    .current_key()
                    .is_some_and(|current| current <= key && current.class_rank() != 3)
                {
                    return None;
                }
                Some((key, target_summary.runnable_count(), target))
            })
            .min_by_key(|(key, load, target)| (*key, *load, target.as_u32()))
            .map(|(_, _, target)| target);
        let Some(target) = target else {
            return Ok(None);
        };
        self.transfer_owner_balance_candidate(
            cpu.as_mut(),
            target,
            task_runtime::monotonic_ns(),
            BalanceReason::RtDeadlinePush,
        )
    }

    /// Replenishes a throttled Deadline job and enqueues it on an owner CPU.
    pub fn replenish_deadline(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let core = {
            let state = self.state.lock();
            Arc::clone(&state.thread_record(thread)?.core)
        };
        {
            let mut sched = core.sched().lock();
            let mut deadline = sched.base_deadline.ok_or(TaskError::NotReady)?;
            deadline.replenish(now_ns);
            if deadline.is_throttled() {
                return Err(TaskError::NotReady);
            }
            match sched.lifecycle.state() {
                ThreadState::Blocked => {
                    sched.transition(&core, ThreadState::Waking)?;
                    sched.transition(&core, ThreadState::Ready)?;
                }
                ThreadState::Waking => sched.transition(&core, ThreadState::Ready)?,
                ThreadState::Ready => {}
                _ => return Err(TaskError::NotReady),
            }
            sched.base_deadline = Some(deadline);
            sched.base_entity = SchedulingEntity::Deadline(deadline);
            if !sched.is_pi_boosted() {
                sched.entity = sched.base_entity;
            }
            sched.deadline_replenish_pending = false;
        }
        self.enqueue_owner_thread(cpu.as_mut(), core, now_ns, EnqueueReason::Replenished)?;
        Self::program_local_timer(cpu.as_mut(), now_ns)
    }

    /// Charges the current dispatch and reports class budget expiration.
    pub fn charge_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        let charge = cpu
            .as_mut()
            .charge_current_dispatch(now_ns, runtime_ns, reclaimed_ns)?;
        Ok(ChargeOutcome {
            slice_expired: charge.slice_expired,
            deadline_overrun: charge.deadline_overrun,
        })
    }

    /// Charges exactly the unaccounted runtime since the current dispatch began
    /// or was last sampled.
    pub fn charge_current_until(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        let charge = cpu.as_mut().settle_current_dispatch(now_ns, reclaimed_ns)?;
        Ok(ChargeOutcome {
            slice_expired: charge.slice_expired,
            deadline_overrun: charge.deadline_overrun,
        })
    }

    /// Tests RT bandwidth, allowing a PI-boosted owner to run to unlock.
    pub fn rt_may_run(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        pi_boosted_owner: bool,
    ) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.ensure_owner_cpu_online(&cpu)?;
        Ok(cpu
            .as_mut()
            .fields_mut()
            .rt_bandwidth
            .may_run(now_ns, pi_boosted_owner))
    }

    /// Selects the next thread according to strict class precedence.
    pub fn schedule(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.ensure_owner_cpu_online(&cpu)?;
        cpu.as_mut().scheduler_enter();
        self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
        self.service_deadline_timers(cpu.as_mut(), now_ns)?;
        // Claim the deadline-work doorbell reasserted by the entry-side
        // pending check. The Acquire recheck inside `scheduler_enter` keeps
        // concurrently published inbox or deadline work sticky.
        cpu.as_mut().scheduler_enter();
        let previous = cpu.current();
        let previous_core = cpu.current_core().cloned();
        let mut migration_target = None;
        if let Some(core) = previous_core.as_ref() {
            migration_target = self.schedule_out_owner_running(
                cpu.as_mut(),
                Arc::clone(core),
                now_ns,
                EnqueueReason::Preempted,
            )?;
        }
        let next = self.pick_owner_next(cpu.as_mut(), now_ns, previous)?;
        if let Some(target) = next.outgoing_migration_target {
            migration_target = Some(target);
        }
        let next_core = next.core;
        Self::stage_switch_handoff(
            cpu.as_mut(),
            previous,
            previous_core.as_ref().map(Arc::clone),
            next_core.id(),
            migration_target,
        )?;
        let reason = if migration_target.is_some() {
            SwitchReason::Migrated
        } else {
            SwitchReason::Preempted
        };
        let decision = Self::owner_switch_plan(previous_core.as_ref(), &next_core, reason);
        Ok(self.finish_owner_selection(cpu, decision, now_ns))
    }

    /// Services sticky scheduler work and switches only for a real preemption.
    pub fn schedule_if_requested(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<SchedulerOutcome, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.ensure_owner_cpu_online(&cpu)?;
        if cpu.current_lifecycle_state() == Some(ThreadState::Parking) {
            // The interrupted owner still holds a generation-checked park
            // token and remains `current` / `on_cpu`. Consume this safe-point
            // doorbell so an IRQ-return `while need_resched` loop can return to
            // `commit_park`. A real preemption request is kept separately and
            // restored only if the park is cancelled.
            let preempt_requested = cpu.as_mut().scheduler_enter();
            cpu.defer_park_preemption(preempt_requested);
            return Ok(SchedulerOutcome::ParkingDeferred);
        }
        let mut switch_requested = cpu.as_mut().scheduler_enter();
        self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
        self.service_deadline_timers(cpu.as_mut(), now_ns)?;
        // Work published while this bounded safe point is running must affect
        // this decision. `scheduler_enter` consumes only the request observed
        // on entry; the second exchange closes the publication window without
        // losing a request that races after it.
        switch_requested |= cpu.as_mut().scheduler_enter();
        let previous = cpu.current();
        let previous_core = cpu.current_core().cloned();
        if let Some(core) = previous_core.as_ref()
            && !switch_requested
        {
            let dispatch = {
                let sched = core.sched().lock();
                Self::owner_dispatch(core, &sched, now_ns)?
            };
            cpu.as_mut().install_dispatch(dispatch);
            self.publish_owner_cpu_load_summary(cpu.as_mut());
            // `scheduler_enter` consumed the sticky entry request, but a
            // bounded inbox drain may have left another batch behind. Preserve
            // that work (and any request produced by Deadline servicing) for
            // the next scheduler safe point.
            if cpu.has_remote_work() {
                cpu.request_scheduler_work();
            }
            Self::program_local_timer(cpu.as_mut(), now_ns)?;
            return Ok(if cpu.needs_reschedule() || cpu.has_remote_work() {
                SchedulerOutcome::OwnerWorkPending
            } else {
                SchedulerOutcome::Quiescent
            });
        }
        let mut migration_target = None;
        if let Some(core) = previous_core.as_ref() {
            migration_target = self.schedule_out_owner_running(
                cpu.as_mut(),
                Arc::clone(core),
                now_ns,
                EnqueueReason::Preempted,
            )?;
        }
        let next = self.pick_owner_next(cpu.as_mut(), now_ns, previous)?;
        if let Some(target) = next.outgoing_migration_target {
            migration_target = Some(target);
        }
        let next_core = next.core;
        Self::stage_switch_handoff(
            cpu.as_mut(),
            previous,
            previous_core.as_ref().map(Arc::clone),
            next_core.id(),
            migration_target,
        )?;
        let reason = if migration_target.is_some() {
            SwitchReason::Migrated
        } else {
            SwitchReason::Preempted
        };
        let decision = Self::owner_switch_plan(previous_core.as_ref(), &next_core, reason);
        Ok(SchedulerOutcome::Decision(
            self.finish_owner_selection(cpu, decision, now_ns),
        ))
    }

    /// Moves the current thread to its class tail and selects another thread.
    pub fn yield_current(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.complete_context_switch(cpu.as_mut())?;
        self.drain_owner_work(cpu.as_mut(), now_ns)?;
        self.ensure_owner_cpu_online(&cpu)?;
        cpu.as_mut().scheduler_enter();
        self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
        self.service_deadline_timers(cpu.as_mut(), now_ns)?;
        cpu.as_mut().scheduler_enter();
        let previous = cpu.current();
        let previous_core = cpu.current_core().cloned();
        let mut migration_target = None;
        if let Some(core) = previous_core.as_ref() {
            let deadline_job_ended = {
                let mut sched = core.sched().lock();
                if matches!(sched.active_base_policy, SchedulePolicy::Deadline(_))
                    && !sched.is_pi_boosted()
                {
                    if !sched.entity.yield_deadline_job() {
                        return Err(TaskError::InvalidConfiguration);
                    }
                    if let SchedulingEntity::Deadline(deadline) = sched.entity {
                        sched.base_entity = sched.entity;
                        sched.base_deadline = Some(deadline);
                    }
                    sched.placement.set_running_cpu(None)?;
                    sched.deadline_replenish_pending = true;
                    sched.transition(core, ThreadState::Blocked)?;
                    Self::refresh_owner_deadline_timers_locked(core, &mut sched, cpu.as_mut())?;
                    true
                } else {
                    false
                }
            };
            if deadline_job_ended {
                Self::mark_owner_deadline_non_contending(core, cpu.as_mut(), now_ns)?;
                cpu.as_mut().clear_current();
            } else {
                migration_target = self.schedule_out_owner_running(
                    cpu.as_mut(),
                    Arc::clone(core),
                    now_ns,
                    EnqueueReason::Yield,
                )?;
            }
        }
        let next = self.pick_owner_next(cpu.as_mut(), now_ns, previous)?;
        if let Some(target) = next.outgoing_migration_target {
            migration_target = Some(target);
        }
        let next_core = next.core;
        Self::stage_switch_handoff(
            cpu.as_mut(),
            previous,
            previous_core.as_ref().map(Arc::clone),
            next_core.id(),
            migration_target,
        )?;
        let decision =
            Self::owner_switch_plan(previous_core.as_ref(), &next_core, SwitchReason::Yield);
        Ok(self.finish_owner_selection(cpu, decision, now_ns))
    }
}
