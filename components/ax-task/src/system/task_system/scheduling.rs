//! Owner scheduling entry points, runtime charging, and load balancing requests.

use super::{dispatch::OwnerDispatchCommit, *};
use crate::{PickTaskResult, RtEligibility};

pub(crate) fn lone_current_yield_keeps_dispatch(
    entity: Option<&SchedulingEntity>,
    rt_effectively_throttled: bool,
) -> bool {
    match entity {
        Some(SchedulingEntity::Fair(_)) => true,
        Some(SchedulingEntity::Fifo | SchedulingEntity::RoundRobin { .. }) => {
            !rt_effectively_throttled
        }
        Some(SchedulingEntity::KernelStop | SchedulingEntity::Deadline(_)) | None => false,
    }
}

fn realtime_current_remains_selected(transaction: &mut OwnerRqTxn<'_>) -> bool {
    if !lone_current_yield_keeps_dispatch(
        transaction.current_scheduling_entity(),
        transaction.rt_is_effectively_throttled(),
    ) {
        return false;
    }
    let Some((current, priority)) = transaction.current().and_then(|current| {
        current
            .schedule_policy()
            .rt_priority()
            .map(|priority| (current.thread(), priority.get()))
    }) else {
        return false;
    };
    if transaction.highest_rt_priority() != Some(priority)
        || transaction.rt_count_at_priority(priority) != 1
    {
        return false;
    }

    // Ask the same class chain used by the eventual owner selection. The
    // unique highest-priority RT node is current, but Stop or Deadline may
    // still precede it. Lower-priority RT and Fair tasks cannot displace it.
    let Some(PickTaskResult::Continue(picked)) =
        transaction.pick_next_task(RtEligibility::Runnable, false)
    else {
        task_runtime::fatal_invariant(0x5343_1217, current.as_u64() as usize)
    };
    let remains_selected = picked.id() == current;
    transaction.rollback_pick(picked);
    remains_selected
}

fn owner_yield_keeps_current_dispatch(transaction: &mut OwnerRqTxn<'_>) -> bool {
    match transaction.current_scheduling_entity() {
        Some(SchedulingEntity::Fair(_)) => transaction.nr_queued() == 0,
        Some(SchedulingEntity::Fifo | SchedulingEntity::RoundRobin { .. }) => {
            // Linux `yield_task_rt()` moves current only within its own
            // priority list. With no peer at that priority, the normal class
            // pick still selects current unless a higher class is runnable.
            realtime_current_remains_selected(transaction)
        }
        Some(SchedulingEntity::KernelStop | SchedulingEntity::Deadline(_)) | None => false,
    }
}

struct RequestedPreemptionCommit {
    decision: ScheduleDecision,
    dispatch: OwnerDispatchCommit,
    deadline_rq_observation: SchedulerDeadlineRqObservation,
    wall_now_ns: u64,
}

struct RequestedPreemptionState {
    previous: Option<ThreadId>,
    previous_core: Option<Arc<ThreadCore>>,
    previous_endpoint: Option<SwitchEndpoint>,
    dispatch: OwnerDispatchCommit,
    migration: Option<PreparedMigrationDelivery>,
    now_ns: u64,
}

impl TaskSystem {
    fn commit_requested_preemption_in_rq(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        mut transaction: OwnerRqTxn<'_>,
        state: RequestedPreemptionState,
    ) -> RequestedPreemptionCommit {
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1206, next_core.id().as_u64() as usize)
        });
        let migrated = state.migration.is_some();
        Self::stage_switch_handoff(
            cpu.as_mut(),
            state.previous,
            state.previous_core.as_ref().map(Arc::clone),
            Arc::clone(&next_core),
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
            !migrated && !state.dispatch.has_deferred_task_lock_work(),
        );
        let decision = Self::owner_switch_plan(
            state.previous_core.as_ref(),
            state.previous_endpoint,
            &next_core,
            next_endpoint,
            reason,
            state.now_ns,
        );
        RequestedPreemptionCommit {
            decision,
            dispatch: state.dispatch,
            deadline_rq_observation,
            wall_now_ns: state.now_ns,
        }
    }

    fn finish_requested_preemption(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        commit: RequestedPreemptionCommit,
    ) -> SchedulerOutcome {
        self.finish_owner_dispatch_commit(cpu.as_mut(), commit.dispatch, commit.wall_now_ns);
        SchedulerOutcome::Decision(self.finish_owner_selection(
            cpu.as_mut(),
            commit.decision,
            commit.deadline_rq_observation,
        ))
    }

    fn lone_realtime_preemption_keeps_dispatch(
        &self,
        transaction: &mut OwnerRqTxn<'_>,
        current: &ThreadCore,
    ) -> bool {
        self.owner_schedule_out_is_rq_owned(transaction, current)
            && realtime_current_remains_selected(transaction)
    }

    fn finish_owner_no_switch(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        mut transaction: OwnerRqTxn<'_>,
        current: &ThreadCore,
        request_scope: SchedulerRequestScope,
    ) -> Result<SchedulerOutcome, TaskError> {
        let runtime_overrun_work = self.sync_owner_current_dispatch_in_rq(&mut transaction);
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit_and_finish_scheduler_request();
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::complete_no_switch_thread_lock_probe(current.id());
        if let Some(core) = runtime_overrun_work {
            self.publish_deadline_overrun_work(core);
        }
        let run_queue_changed =
            if self.owner_balance_work_pending(cpu.as_ref().get_ref(), current.id()) {
                self.service_owner_balance(cpu.as_mut(), current.id())?
                    .run_queue_changed()
            } else {
                false
            };
        if run_queue_changed {
            self.program_local_timer(
                cpu.as_mut(),
                SchedulerDeadlineDerivationSource::ScheduleNoSwitch,
            )?;
        } else {
            self.program_local_timer_from_rq_observation(
                cpu.as_mut(),
                deadline_rq_observation,
                SchedulerDeadlineDerivationSource::ScheduleNoSwitch,
            )?;
        }
        Ok(
            if cpu.scheduler_request_pending(request_scope) || cpu.has_remote_work() {
                SchedulerOutcome::OwnerWorkPending
            } else {
                SchedulerOutcome::Quiescent
            },
        )
    }

    /// Requests one owner-mediated pull from the busiest remote CPU.
    ///
    /// The target never locks or mutates the source runqueue. Its pinned request
    /// node is published to the source owner-control inbox and the source owner
    /// selects and hands off one affinity-compatible thread at a safe point.
    pub fn request_idle_pull(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if task_runtime::in_hard_irq() {
            return Ok(false);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        if !self.root_domain.has_idle_pull_source() {
            cpu.as_mut().reset_idle_pull_scan();
            return Ok(false);
        }
        if !cpu.idle_pull_eligible() || cpu.has_remote_work() {
            cpu.as_mut().reset_idle_pull_scan();
            return Ok(false);
        }
        let target_remote = Arc::clone(cpu.remote());
        let reservation = match target_remote.begin_idle_pull() {
            IdlePullReservation::Started(reservation) => reservation,
            IdlePullReservation::AlreadyPending => return Ok(true),
            IdlePullReservation::Busy => {
                target_remote.request_idle_pull_retry();
                return Ok(true);
            }
        };
        if !cpu.idle_pull_eligible() || cpu.has_remote_work() {
            target_remote.cancel_idle_pull(reservation);
            cpu.as_mut().reset_idle_pull_scan();
            return Ok(false);
        }
        let target = cpu.owner();
        let source = self
            .root_domain
            .find_idle_pull_source(target, cpu.idle_pull_visited());
        let Some((source, class)) = source else {
            target_remote.cancel_idle_pull(reservation);
            cpu.as_mut().reset_idle_pull_scan();
            return Ok(false);
        };
        cpu.as_mut().mark_idle_pull_source(source);
        let Some(source_local) = self.cpu_remote(source) else {
            target_remote.cancel_idle_pull(reservation);
            cpu.request_scheduler_work();
            return Ok(true);
        };
        let message = InboxMessage::balance_request(source, target, reservation, class);
        let result = source_local.publish_owner_control(cpu.balance_request_node(), message);
        match result {
            PublishResult::Published => Ok(true),
            PublishResult::AlreadyPending => {
                target_remote.cancel_idle_pull(reservation);
                cpu.request_scheduler_work();
                Ok(true)
            }
            PublishResult::WrongKind => {
                target_remote.cancel_idle_pull(reservation);
                cpu.request_scheduler_work();
                Ok(true)
            }
        }
    }

    /// Pushes one queued thread from an overloaded owner to the least loaded CPU.
    ///
    /// Selection and dequeue happen only on `cpu`; the target receives an
    /// intrusive handoff and enqueues it in its own safe-point drain.
    pub fn push_rt_deadline(&self, cpu: Pin<&mut CpuLocal>) -> Result<Option<ThreadId>, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if task_runtime::in_hard_irq() {
            return Ok(None);
        }
        self.ensure_owner_cpu_online(&cpu)?;
        self.push_rt_deadline_from_root_domain(cpu, None)
    }

    /// Pushes from the coherent owner snapshot published by the immediately
    /// preceding runqueue transaction.
    ///
    /// Scheduler selection publishes after installing its next dispatch, so
    /// its common tail can reuse that snapshot just as Linux keeps balancing
    /// decisions under one owner-rq transaction. Callers must not mutate the
    /// local runqueue or current dispatch between publication and this call.
    pub(super) fn push_rt_deadline_from_root_domain(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        class: Option<SchedulingClass>,
    ) -> Result<Option<ThreadId>, TaskError> {
        if !class.map_or_else(
            || self.root_domain.cpu_has_rt_deadline_overload(cpu.owner()),
            |class| self.root_domain.cpu_has_overload(cpu.owner(), class),
        ) {
            return Ok(None);
        }
        let Some(selection) =
            self.select_rt_deadline_balance_transfer(cpu.as_ref().get_ref(), class)
        else {
            return Ok(None);
        };
        let target = selection.target();
        let outcome = self.commit_owner_balance_transfer(cpu.as_mut(), selection)?;
        if outcome == BalanceTransferOutcome::Retry
            && let Some(target_remote) = self.cpu_remote(target)
        {
            // Ask the idle destination to issue a fresh owner-mediated pull.
            // This keeps retry asynchronous instead of spinning the source
            // scheduler tail on a transient affinity/publication race.
            target_remote.kick_scheduler_work();
        }
        Ok(outcome.migrated())
    }

    /// Charges the current dispatch and reports class budget expiration.
    pub fn charge_current(
        &self,
        cpu: Pin<&mut CpuLocal>,
        runtime_ns: u64,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        if transaction.current().is_none() {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        }
        let charge = transaction.charge_current(runtime_ns, reclaimed_ns);
        transaction.commit();
        Ok(ChargeOutcome {
            slice_expired: charge.slice_expired,
            deadline_overrun: charge.deadline_overrun,
        })
    }

    /// Charges exactly the unaccounted runtime since the current dispatch began
    /// or was last sampled.
    pub fn charge_current_until(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
    ) -> Result<ChargeOutcome, TaskError> {
        self.charge_current_until_with_clock(cpu, reclaimed_ns)
            .map(|(charge, _clock, _thread)| charge)
    }

    pub(crate) fn charge_current_until_with_clock(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
    ) -> Result<(ChargeOutcome, RunQueueClockSnapshot, ThreadId), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        let clock = transaction.clock();
        let Some(thread) = transaction.current_thread() else {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        };
        let charge = transaction.settle_current(reclaimed_ns);
        transaction.commit();
        Ok((
            ChargeOutcome {
                slice_expired: charge.slice_expired,
                deadline_overrun: charge.deadline_overrun,
            },
            clock,
            thread,
        ))
    }

    pub(crate) fn task_tick_current_until_with_clock(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
    ) -> Result<(ChargeOutcome, RunQueueClockSnapshot, ThreadId), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        let clock = transaction.clock();
        let Some(thread) = transaction.current_thread() else {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        };
        let charge = transaction.task_tick_current_until(reclaimed_ns);
        transaction.commit();
        Ok((
            ChargeOutcome {
                slice_expired: charge.slice_expired,
                deadline_overrun: charge.deadline_overrun,
            },
            clock,
            thread,
        ))
    }

    /// Reports Linux `!rt_rq_throttled(rq)` for the owner runqueue.
    pub fn rt_run_queue_may_run(&self, cpu: Pin<&mut CpuLocal>) -> Result<bool, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.ensure_owner_cpu_online(&cpu)?;
        let run_queue = cpu
            .remote()
            .lock_run_queue(RunQueueGuardSource::RtAccounting);
        Ok(!run_queue.rt_is_throttled() || run_queue.has_exempt_rt())
    }

    /// Selects the next thread according to strict class precedence.
    ///
    /// `current` is the architecture-published task identity used only to
    /// acquire task-owned scheduler state before the runqueue transaction.
    /// `None` is valid only for an initial dispatch with no `rq->curr`.
    pub fn schedule(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: Option<&ThreadHandle>,
    ) -> Result<ScheduleDecision, TaskError> {
        self.schedule_owner(
            cpu,
            current.map(|thread| thread.runtime_core_arc().as_ref()),
            OwnerRqEntry::IrqSave,
        )
    }

    fn schedule_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: Option<&ThreadCore>,
        rq_entry: OwnerRqEntry,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let remote = Arc::clone(cpu.remote());
        let initial_request = remote.claim_scheduler_request(SchedulerRequestScope::All);
        // SAFETY: propagated from the selected entry contract.
        unsafe { self.complete_context_switch_owner(cpu.as_mut(), rq_entry)? };
        self.drain_owner_work(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let previous_core_hint = current;
        let mut previous_sched = previous_core_hint.map(|core| {
            // SAFETY: propagated from the selected entry contract.
            unsafe { rq_entry.lock_thread_sched(core.sched()) }
        });
        // SAFETY: the public task entry chooses irqsave; the scheduler-frame
        // entry is exposed only by its unsafe wrapper below.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        transaction.adopt_scheduler_request(initial_request);
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        // Runtime accounting is part of this unconditional scheduling
        // decision, exactly like Linux update_curr() preceding pick_next.
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let previous = transaction.current_thread();
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        if previous_core.as_deref().map(core::ptr::from_ref)
            != previous_core_hint.map(core::ptr::from_ref)
        {
            task_runtime::fatal_invariant(0x5343_1201, cpu.owner().as_u32() as usize);
        }
        let mut migration = None;
        if let Some(core) = previous_core.as_ref() {
            let schedule_out = self.schedule_out_owner_running_in_rq(
                cpu.as_mut(),
                &mut transaction,
                Arc::clone(core),
                previous_sched.as_deref_mut().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1202, core.id().as_u64() as usize)
                }),
                now_ns,
                EnqueueReason::Preempted,
            );
            migration = schedule_out.migration;
        }
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1203, next_core.id().as_u64() as usize)
        });
        let migrated = migration.is_some();
        Self::stage_switch_handoff(
            cpu.as_mut(),
            previous,
            previous_core.as_ref().map(Arc::clone),
            Arc::clone(&next_core),
            migration,
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
            !migrated && !dispatch_commit.has_deferred_task_lock_work(),
        );
        drop(previous_sched);
        let decision = Self::owner_switch_plan(
            previous_core.as_ref(),
            previous_endpoint,
            &next_core,
            next_endpoint,
            reason,
            now_ns,
        );
        self.finish_owner_dispatch_commit(cpu.as_mut(), dispatch_commit, clock.wall().as_nanos());
        let decision = self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);
        Ok(decision)
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

    fn schedule_if_requested_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadCore,
        rq_entry: OwnerRqEntry,
        request_scope: SchedulerRequestScope,
    ) -> Result<SchedulerOutcome, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let remote = Arc::clone(cpu.remote());
        let initial_request = remote.claim_scheduler_request(request_scope);
        // SAFETY: propagated from the selected entry contract.
        unsafe { self.complete_context_switch_owner(cpu.as_mut(), rq_entry)? };
        self.drain_owner_work(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let previous_core_hint = current;
        // Probe the rq-owned decision first. Linux's ordinary no-switch pass
        // never acquires p->pi_lock; task scheduler state is needed only after
        // this transaction proves that put_prev_task() will run.
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
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
        if transaction
            .current()
            .map(|dispatch| dispatch.runtime_core_arc().state())
            == Some(ThreadState::Parking)
        {
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
            .current()
            .is_none_or(|dispatch| !core::ptr::eq(dispatch.runtime_core(), previous_core_hint))
        {
            task_runtime::fatal_invariant(0x5343_1204, cpu.owner().as_u32() as usize);
        }
        if !switch_requested
            || self.lone_realtime_preemption_keeps_dispatch(&mut transaction, previous_core_hint)
        {
            return self.finish_owner_no_switch(
                cpu.as_mut(),
                transaction,
                previous_core_hint,
                request_scope,
            );
        }
        if self.owner_schedule_out_is_rq_owned(&transaction, previous_core_hint) {
            let now_ns = transaction.clock().wall().as_nanos();
            let previous_core = transaction.current_core();
            let previous_endpoint = transaction.current_switch_endpoint();
            let dispatch_commit = self.sync_owner_settled_current_dispatch_in_rq(&mut transaction);
            let outgoing = previous_core.as_ref().map(Arc::clone).unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1204, cpu.owner().as_u32() as usize)
            });
            self.schedule_out_owner_rq_owned(
                cpu.as_mut(),
                &mut transaction,
                outgoing,
                EnqueueReason::Preempted,
            );
            let commit = self.commit_requested_preemption_in_rq(
                cpu.as_mut(),
                transaction,
                RequestedPreemptionState {
                    previous,
                    previous_core,
                    previous_endpoint,
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

        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_no_switch_thread_lock(previous_core_hint.id());
        // A real switch follows the established p->pi_lock -> rq order. The
        // second rq pass resamples its clock and revalidates current rather
        // than carrying a stale snapshot across the unlocked interval.
        // SAFETY: propagated from the selected entry contract.
        let mut previous_sched = unsafe { rq_entry.lock_thread_sched(previous_core_hint.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        transaction.adopt_scheduler_request(request);
        if transaction.current().is_some() {
            let _settled = transaction.settle_current(0);
        }
        let request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        if !request.preemption_requested() {
            task_runtime::fatal_invariant(0x5343_120a, cpu.owner().as_u32() as usize);
        }
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
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
                previous_core,
                previous_endpoint,
                dispatch: dispatch_commit,
                migration,
                now_ns,
            },
        );
        drop(previous_sched);
        Ok(self.finish_requested_preemption(cpu.as_mut(), commit))
    }

    /// Moves the current thread to its class tail and selects another thread.
    ///
    /// `current` must be the architecture-published task identity. The owner
    /// runqueue transaction revalidates it against `rq->curr` before use.
    pub fn yield_current(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<ScheduleDecision, TaskError> {
        self.yield_current_owner(cpu, current.runtime_core_arc(), OwnerRqEntry::IrqSave)
    }

    /// Yields while the runtime owns the IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn yield_current_in_scheduler_frame(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &CurrentThreadRef,
    ) -> Result<ScheduleDecision, TaskError> {
        self.yield_current_owner(cpu, current.runtime_core(), OwnerRqEntry::SchedulerFrame)
    }

    fn yield_current_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadCore,
        rq_entry: OwnerRqEntry,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let remote = Arc::clone(cpu.remote());
        let initial_request = remote.claim_scheduler_request(SchedulerRequestScope::All);
        // SAFETY: propagated from the selected entry contract.
        unsafe { self.complete_context_switch_owner(cpu.as_mut(), rq_entry)? };
        self.drain_owner_work(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let previous_core_hint = current;
        // Probe rq ownership before taking the current task lock. Linux's
        // ordinary sched_yield path holds only rq->lock; task state is needed
        // only for migration, Deadline, or other task-control work.
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        transaction.adopt_scheduler_request(initial_request);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::begin_current_dispatch_accounting_probe(current.id());
        if transaction.current().is_some() {
            let _settled = transaction.settle_current(0);
        }
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::complete_current_dispatch_accounting_probe(current.id());
        // A forced yield consumes a slice-expiration request discovered while
        // accounting the outgoing task in this same scheduling pass.
        let request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        if transaction
            .current()
            .is_none_or(|dispatch| !core::ptr::eq(dispatch.runtime_core(), previous_core_hint))
        {
            task_runtime::fatal_invariant(0x5343_1207, cpu.owner().as_u32() as usize);
        }
        if self.owner_schedule_out_is_rq_owned(&transaction, previous_core_hint) {
            return Ok(self.yield_current_rq_owned(cpu.as_mut(), transaction, previous_core_hint));
        }
        // Preserve requests merged by the rq-owned probe while restoring the
        // full p->pi_lock -> rq order for exceptional task-control work.
        transaction.commit();

        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_yield_thread_lock(previous_core_hint.id());
        // SAFETY: propagated from the selected entry contract.
        let mut previous_sched = unsafe { rq_entry.lock_thread_sched(previous_core_hint.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        transaction.adopt_scheduler_request(request);
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let previous = transaction.current_thread();
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        if previous_core
            .as_ref()
            .is_none_or(|core| !core::ptr::eq(core.as_ref(), previous_core_hint))
        {
            task_runtime::fatal_invariant(0x5343_1207, cpu.owner().as_u32() as usize);
        }
        if let Some(core) = previous_core.as_ref() {
            let owner = cpu.owner();
            let continuing_dispatch = {
                owner_yield_keeps_current_dispatch(&mut transaction)
                    && transaction
                        .task_state(core.id(), core.sched().placement())
                        .is_current()
                    && core.sched().placement().requested_migration().is_none()
                    && previous_sched.affinity.affinity.contains(owner)
            };
            if continuing_dispatch {
                // Linux `yield_task_fair()` returns immediately for a lone
                // Fair task. `yield_task_rt()` moves a lone FIFO/RR list node
                // to the same list tail, then `pick_next_task()` selects the
                // unchanged `rq->curr`; `put_prev_set_next_task()` therefore
                // performs no lifecycle transition. Keep the current
                // dispatch in both cases instead of manufacturing a
                // Running -> Ready -> Running cycle. Effective RT throttling
                // remains a real reason to leave the current dispatch.
                #[cfg(feature = "task-test-hooks")]
                crate::task_test_hooks::record_lone_yield_runtime_state(
                    core.id(),
                    core.runtime_snapshot(Some(
                        transaction
                            .current()
                            .unwrap_or_else(|| {
                                task_runtime::fatal_invariant(
                                    0x5343_1211,
                                    core.id().as_u64() as usize,
                                )
                            })
                            .runtime_interval_ns(clock.task().as_nanos()),
                    ))
                    .is_running(),
                );
                let deadline_rq_observation =
                    transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
                transaction.commit_and_finish_scheduler_request();
                drop(previous_sched);
                let endpoint = previous_endpoint.unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1209, core.id().as_u64() as usize)
                });
                let decision = Self::owner_switch_plan(
                    Some(core),
                    Some(endpoint),
                    core,
                    endpoint,
                    SwitchReason::Yield,
                    now_ns,
                );
                self.finish_owner_dispatch_commit(
                    cpu.as_mut(),
                    dispatch_commit,
                    clock.wall().as_nanos(),
                );
                let decision =
                    self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);
                #[cfg(feature = "task-test-hooks")]
                crate::task_test_hooks::complete_yield_thread_lock_probe(core.id());
                return Ok(decision);
            }
        }
        let mut migration = None;
        if let Some(core) = previous_core.as_ref() {
            let deadline_job_ended = {
                let placement = core.sched().placement();
                let sched = &mut previous_sched;
                if matches!(sched.policy.base, SchedulePolicy::Deadline(_))
                    && !sched.is_pi_boosted()
                {
                    if sched.lifecycle.state() != ThreadState::Running
                        || placement.queued_cpu() != Some(cpu.owner())
                        || placement.on_cpu() != Some(cpu.owner())
                    {
                        task_runtime::fatal_invariant(0x5343_120b, core.id().as_u64() as usize);
                    }
                    let current_entity = transaction
                        .current_scheduling_entity_mut()
                        .unwrap_or_else(|| {
                            task_runtime::fatal_invariant(0x5343_120c, core.id().as_u64() as usize)
                        });
                    if !current_entity.yield_deadline_job() {
                        task_runtime::fatal_invariant(0x5343_120d, core.id().as_u64() as usize);
                    }
                    transaction
                        .throttle_current_deadline(core.id())
                        .unwrap_or_else(|_| {
                            task_runtime::fatal_invariant(0x5343_120e, core.id().as_u64() as usize)
                        });
                    placement.put_prev(cpu.owner());
                    if self
                        .refresh_owner_deadline_timers_in_rq(
                            core,
                            sched,
                            cpu.as_mut(),
                            now_ns,
                            &mut transaction,
                        )
                        .is_some()
                    {
                        cpu.request_scheduler_work();
                    }
                    true
                } else {
                    false
                }
            };
            if deadline_job_ended {
                transaction.take_current();
            } else {
                let schedule_out = self.schedule_out_owner_running_in_rq(
                    cpu.as_mut(),
                    &mut transaction,
                    Arc::clone(core),
                    &mut previous_sched,
                    now_ns,
                    EnqueueReason::Yield,
                );
                migration = schedule_out.migration;
            }
        }
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1210, next_core.id().as_u64() as usize)
        });
        let migrated = migration.is_some();
        Self::stage_switch_handoff(
            cpu.as_mut(),
            previous,
            previous_core.as_ref().map(Arc::clone),
            Arc::clone(&next_core),
            migration,
        );
        let reason = if migrated {
            SwitchReason::Migrated
        } else {
            SwitchReason::Yield
        };
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        self.commit_owner_switch_selection(
            cpu.as_mut(),
            transaction,
            !migrated && !dispatch_commit.has_deferred_task_lock_work(),
        );
        drop(previous_sched);
        let decision = Self::owner_switch_plan(
            previous_core.as_ref(),
            previous_endpoint,
            &next_core,
            next_endpoint,
            reason,
            now_ns,
        );
        self.finish_owner_dispatch_commit(cpu.as_mut(), dispatch_commit, clock.wall().as_nanos());
        let decision = self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::complete_yield_thread_lock_probe(previous_core_hint.id());
        Ok(decision)
    }

    /// Implements Linux's rq-owned ordinary yield path.
    fn yield_current_rq_owned(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        mut transaction: OwnerRqTxn<'_>,
        previous_core_hint: &ThreadCore,
    ) -> ScheduleDecision {
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        let dispatch_commit = self.sync_owner_settled_current_dispatch_in_rq(&mut transaction);
        let previous = transaction.current_thread();
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        if previous_core
            .as_ref()
            .is_none_or(|core| !core::ptr::eq(core.as_ref(), previous_core_hint))
        {
            task_runtime::fatal_invariant(0x5343_1212, cpu.owner().as_u32() as usize);
        }

        let core = previous_core.as_ref().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1213, cpu.owner().as_u32() as usize)
        });
        let placement = core.sched().placement();
        let continuing_dispatch = owner_yield_keeps_current_dispatch(&mut transaction)
            && transaction.task_state(core.id(), placement).is_current();
        if continuing_dispatch {
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::record_lone_yield_runtime_state(
                core.id(),
                core.runtime_snapshot(Some(
                    transaction
                        .current()
                        .unwrap_or_else(|| {
                            task_runtime::fatal_invariant(0x5343_1214, core.id().as_u64() as usize)
                        })
                        .runtime_interval_ns(clock.task().as_nanos()),
                ))
                .is_running(),
            );
            let deadline_rq_observation =
                transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
            transaction.commit_and_finish_scheduler_request();
            let endpoint = previous_endpoint.unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1215, core.id().as_u64() as usize)
            });
            let decision = Self::owner_switch_plan(
                Some(core),
                Some(endpoint),
                core,
                endpoint,
                SwitchReason::Yield,
                now_ns,
            );
            self.finish_owner_dispatch_commit(
                cpu.as_mut(),
                dispatch_commit,
                clock.wall().as_nanos(),
            );
            let decision =
                self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::complete_yield_thread_lock_probe(core.id());
            return decision;
        }

        self.schedule_out_owner_rq_owned(
            cpu.as_mut(),
            &mut transaction,
            Arc::clone(core),
            EnqueueReason::Yield,
        );
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None);
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1216, next_core.id().as_u64() as usize)
        });
        Self::stage_switch_handoff(
            cpu.as_mut(),
            previous,
            previous_core.as_ref().map(Arc::clone),
            Arc::clone(&next_core),
            None,
        );
        let deadline_rq_observation =
            transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        self.commit_owner_switch_selection(
            cpu.as_mut(),
            transaction,
            !dispatch_commit.has_deferred_task_lock_work(),
        );
        let decision = Self::owner_switch_plan(
            previous_core.as_ref(),
            previous_endpoint,
            &next_core,
            next_endpoint,
            SwitchReason::Yield,
            now_ns,
        );
        self.finish_owner_dispatch_commit(cpu.as_mut(), dispatch_commit, clock.wall().as_nanos());
        let decision = self.finish_owner_selection(cpu.as_mut(), decision, deadline_rq_observation);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::complete_yield_thread_lock_probe(previous_core_hint.id());
        decision
    }
}
