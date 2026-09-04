//! Owner scheduling entry points, runtime charging, and load balancing requests.

use super::{
    dispatch::OwnerDispatchCommit,
    switch::{OwnerRqScheduleOut, OwnerRqScheduledOut},
    *,
};
use crate::{
    RtEligibility, SchedulerClass,
    system::cpu::{PreviousSwitchDisposition, SchedulerRequestClaim},
};

fn realtime_current_remains_selected(transaction: &mut OwnerRqTxn<'_>) -> bool {
    if transaction.rt_is_effectively_throttled() {
        return false;
    }
    let Some(priority) = transaction.current().and_then(|current| {
        current
            .schedule_policy()
            .rt_priority()
            .map(|priority| priority.get())
    }) else {
        return false;
    };
    if transaction.highest_rt_priority() != Some(priority)
        || transaction.rt_count_at_priority(priority) != 1
    {
        return false;
    }

    // The unique highest-priority RT node is current. Linux next checks the
    // static class chain: only queued Stop or eligible Deadline work can
    // displace it. Do not mutate and roll back class queues merely to prove
    // that no higher class is selectable.
    !transaction.has_selectable_higher_class(SchedulerClass::Realtime, RtEligibility::Runnable)
}

fn owner_yield_keeps_current_dispatch(transaction: &mut OwnerRqTxn<'_>) -> bool {
    let Some(policy) = transaction
        .current()
        .map(|current| current.schedule_policy())
    else {
        return false;
    };
    match SchedulerClass::for_policy(policy) {
        SchedulerClass::Fair => transaction.nr_queued() == 0,
        SchedulerClass::Realtime => {
            // Linux reads `rq->curr->sched_class` directly. The effective
            // policy belongs to the same rq-owned current dispatch, so class
            // selection must not rediscover its entity through the queued
            // generation index.
            realtime_current_remains_selected(transaction)
        }
        SchedulerClass::Stop | SchedulerClass::Deadline => false,
    }
}

struct RequestedPreemptionCommit {
    decision: ScheduleDecision,
    previous_urgency: Option<SchedulingUrgency>,
    next_urgency: SchedulingUrgency,
    dispatch: OwnerDispatchCommit,
    deadline_rq_observation: SchedulerDeadlineRqObservation,
}

struct RequestedPreemptionState {
    previous: Option<ThreadId>,
    previous_core: Option<Arc<ThreadCore>>,
    previous_endpoint: Option<SwitchEndpoint>,
    previous_urgency: Option<SchedulingUrgency>,
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
        let next = self.pick_owner_next_after_preemption_in_rq(
            cpu.as_mut(),
            &mut transaction,
            state.previous,
        );
        let OwnerNext {
            core: next_core,
            policy: next_policy,
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
            next_policy,
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

    fn finish_requested_preemption(
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

    fn lone_realtime_preemption_keeps_dispatch(
        &self,
        transaction: &mut OwnerRqTxn<'_>,
        current: &ThreadCore,
    ) -> bool {
        realtime_current_remains_selected(transaction)
            && self
                .prepare_owner_rq_schedule_out(transaction, current)
                .is_some()
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
            IdlePullReservation::Busy => return Ok(false),
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

    /// Lets the selected Linux-style ILB coordinate one Fair pull for every
    /// CPU still published in the root-domain idle mask.
    ///
    /// The coordinator only reserves target-owned request nodes and publishes
    /// them to source owners. Source and target runqueues remain private to
    /// their owners; a failed or stale request ends this NOHZ pass instead of
    /// kicking the target into an immediate retry loop.
    pub(super) fn request_fair_nohz_idle_pulls(&self) -> bool {
        let mut requested = false;
        for (index, target_remote) in self.cpu_remotes.iter().enumerate() {
            let target = CpuId::new(index as u32);
            if !self.root_domain.fair_nohz_idle_target(target)
                || !target_remote.accepts_placement()
                || !target_remote.is_scheduler_ready()
            {
                continue;
            }
            requested |= self.request_fair_nohz_idle_pull(target, target_remote);
        }
        requested
    }

    fn request_fair_nohz_idle_pull(&self, target: CpuId, target_remote: &CpuRemote) -> bool {
        let reservation = match target_remote.begin_idle_pull() {
            IdlePullReservation::Started(reservation) => reservation,
            IdlePullReservation::AlreadyPending => return true,
            IdlePullReservation::Busy => return false,
        };
        if !self.root_domain.fair_nohz_idle_target(target)
            || !target_remote.accepts_placement()
            || !target_remote.is_scheduler_ready()
        {
            target_remote.cancel_idle_pull(reservation);
            return false;
        }
        let Some(source) = self
            .root_domain
            .find_unvisited_fair_idle_pull_source(target)
        else {
            target_remote.cancel_idle_pull(reservation);
            return false;
        };
        let Some(source_remote) = self.cpu_remote(source) else {
            target_remote.cancel_idle_pull(reservation);
            return false;
        };
        let message =
            InboxMessage::balance_request(source, target, reservation, SchedulingClass::Fair);
        match source_remote.publish_owner_control(target_remote.balance_request_node(), message) {
            PublishResult::Published => true,
            PublishResult::AlreadyPending | PublishResult::WrongKind => {
                target_remote.cancel_idle_pull(reservation);
                false
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
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and all dispatch-tail
        // mutations are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let mut transaction = OwnerRqTxn::begin(self, remote);
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
            .map(|(charge, _clock, _thread, _rq_observation)| charge)
    }

    pub(crate) fn charge_current_until_with_clock(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
    ) -> Result<
        (
            ChargeOutcome,
            RunQueueClockSnapshot,
            ThreadId,
            SchedulerDeadlineRqObservation,
        ),
        TaskError,
    > {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and all dispatch-tail
        // mutations are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let mut transaction = OwnerRqTxn::begin(self, remote);
        let clock = transaction.clock();
        let Some(thread) = transaction.current_thread() else {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        };
        let charge = transaction.settle_current(reclaimed_ns);
        let rq_observation = transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit();
        Ok((
            ChargeOutcome {
                slice_expired: charge.slice_expired,
                deadline_overrun: charge.deadline_overrun,
            },
            clock,
            thread,
            rq_observation,
        ))
    }

    pub(crate) fn task_tick_current_until_with_clock(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
        tick_ns: u64,
    ) -> Result<
        (
            ChargeOutcome,
            RunQueueClockSnapshot,
            ThreadId,
            SchedulerDeadlineRqObservation,
        ),
        TaskError,
    > {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and all dispatch-tail
        // mutations are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let mut transaction = OwnerRqTxn::begin(self, remote);
        let clock = transaction.clock();
        let Some(thread) = transaction.current_thread() else {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        };
        let charge = transaction.task_tick_current_until(reclaimed_ns, tick_ns);
        let rq_observation = transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit();
        Ok((
            ChargeOutcome {
                slice_expired: charge.slice_expired,
                deadline_overrun: charge.deadline_overrun,
            },
            clock,
            thread,
            rq_observation,
        ))
    }

    pub(crate) fn clock_event_current_until_with_clock(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
    ) -> Result<
        (
            ChargeOutcome,
            RunQueueClockSnapshot,
            ThreadId,
            SchedulerDeadlineRqObservation,
        ),
        TaskError,
    > {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint for the complete accounting transaction.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let mut transaction = OwnerRqTxn::begin(self, remote);
        let clock = transaction.clock();
        let Some(thread) = transaction.current_thread() else {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        };
        let charge = transaction.clock_event_current_until(reclaimed_ns);
        let rq_observation = transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit();
        Ok((
            ChargeOutcome {
                slice_expired: charge.slice_expired,
                deadline_overrun: charge.deadline_overrun,
            },
            clock,
            thread,
            rq_observation,
        ))
    }

    pub(crate) fn task_tick_and_clock_event_current_until_with_clock(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reclaimed_ns: u64,
        tick_ns: u64,
    ) -> Result<
        (
            ChargeOutcome,
            RunQueueClockSnapshot,
            ThreadId,
            SchedulerDeadlineRqObservation,
        ),
        TaskError,
    > {
        self.ensure_owner_cpu_context(&cpu)?;
        if !cpu.is_online() {
            return Err(TaskError::CpuOffline(cpu.owner().as_u32()));
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint for the complete accounting transaction.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let mut transaction = OwnerRqTxn::begin(self, remote);
        let clock = transaction.clock();
        let Some(thread) = transaction.current_thread() else {
            transaction.commit();
            return Err(TaskError::NoRunnableThread);
        };
        let charge = transaction.task_tick_and_clock_event_current_until(reclaimed_ns, tick_ns);
        let rq_observation = transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
        transaction.commit();
        Ok((
            ChargeOutcome {
                slice_expired: charge.slice_expired,
                deadline_overrun: charge.deadline_overrun,
            },
            clock,
            thread,
            rq_observation,
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
        let validate_owner = rq_entry.requires_owner_context_validation();
        if validate_owner {
            self.ensure_owner_cpu_context(&cpu)?;
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and switch tail are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        let initial_request = remote.claim_scheduler_request(SchedulerRequestScope::All);
        self.drain_owner_work(cpu.as_mut())?;
        if validate_owner {
            self.ensure_owner_cpu_registration_online(&cpu)?;
        }
        let previous_core_hint = current;
        let mut previous_sched = previous_core_hint.map(|core| {
            // SAFETY: propagated from the selected entry contract.
            unsafe { rq_entry.lock_thread_sched(core.sched()) }
        });
        // SAFETY: the public task entry chooses irqsave; the scheduler-frame
        // entry is exposed only by its unsafe wrapper below.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        let now_ns = transaction.clock().wall().as_nanos();
        transaction.adopt_scheduler_request(initial_request);
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        // Runtime accounting is part of this unconditional scheduling
        // decision, exactly like Linux update_curr() preceding pick_next.
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let previous = transaction.current_thread();
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        let previous_urgency = transaction.current_scheduling_urgency();
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
        let next =
            self.pick_owner_next_after_preemption_in_rq(cpu.as_mut(), &mut transaction, previous);
        let OwnerNext {
            core: next_core,
            policy: next_policy,
            urgency: next_urgency,
        } = next;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1203, next_core.as_ref().id().as_u64() as usize)
        });
        let migrated = migration.is_some();
        let handoff = Self::prepare_switch_handoff(
            previous,
            previous_core,
            next_core,
            next_policy,
            PreviousSwitchDisposition::Live,
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
            handoff,
            !migrated && !dispatch_commit.has_deferred_task_lock_work(),
        );
        drop(previous_sched);
        let decision = Self::owner_switch_plan(previous_endpoint, next_endpoint, reason, now_ns);
        self.finish_owner_dispatch_commit(dispatch_commit);
        self.finish_owner_selection(
            cpu.as_mut(),
            decision.previous(),
            decision.next(),
            previous_urgency,
            next_urgency,
            OwnerSchedulerDeadline::Reevaluate(deadline_rq_observation),
        );
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
            return self.finish_owner_no_switch(
                cpu.as_mut(),
                transaction,
                previous_core_hint,
                request_scope,
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
                policy: _,
                urgency: previous_urgency,
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
                previous_core,
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

    /// Moves the current thread to its class tail and selects another thread.
    ///
    /// `current` must be the architecture-published task identity. The owner
    /// runqueue transaction revalidates it against `rq->curr` before use.
    pub fn yield_current(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<YieldOutcome, TaskError> {
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
    ) -> Result<YieldOutcome, TaskError> {
        self.yield_current_owner(cpu, current.runtime_core(), OwnerRqEntry::SchedulerFrame)
    }

    fn yield_current_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadCore,
        rq_entry: OwnerRqEntry,
    ) -> Result<YieldOutcome, TaskError> {
        #[cfg(feature = "qperf-metrics")]
        let owner_entry_started_ns = task_runtime::monotonic_now().as_nanos();
        let validate_owner = rq_entry.requires_owner_context_validation();
        if validate_owner {
            self.ensure_owner_cpu_context(&cpu)?;
        }
        // SAFETY: the owner borrow pins the CpuLocal and its immutable remote
        // endpoint while this scheduling transaction and switch tail are live.
        let remote = unsafe { cpu.as_ref().get_ref().remote_for_owner() };
        self.drain_owner_work(cpu.as_mut())?;
        #[cfg(feature = "qperf-metrics")]
        let owner_drain_finished_ns = task_runtime::monotonic_now().as_nanos();
        if validate_owner {
            self.ensure_owner_cpu_registration_online(&cpu)?;
        }
        let previous_core_hint = current;
        // Probe rq ownership before taking the current task lock. Linux's
        // ordinary sched_yield path holds only rq->lock; task state is needed
        // only for migration, Deadline, or other task-control work.
        // SAFETY: propagated from the selected entry contract.
        #[cfg(feature = "qperf-metrics")]
        let rq_begin_started_ns = task_runtime::monotonic_now().as_nanos();
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        #[cfg(feature = "qperf-metrics")]
        let rq_begin_finished_ns = task_runtime::monotonic_now().as_nanos();
        if transaction
            .current_core_ref()
            .is_none_or(|current| !core::ptr::eq(current, previous_core_hint))
        {
            task_runtime::fatal_invariant(0x5343_1207, cpu.owner().as_u32() as usize);
        }
        if let Some(schedule_out) =
            self.prepare_owner_rq_schedule_out(&transaction, previous_core_hint)
        {
            #[cfg(feature = "qperf-metrics")]
            {
                let rq_preflight_finished_ns = task_runtime::monotonic_now().as_nanos();
                crate::metrics::qperf_record_switch_scheduler_detail(
                    10,
                    owner_entry_started_ns,
                    owner_drain_finished_ns,
                );
                crate::metrics::qperf_record_switch_scheduler_detail(
                    11,
                    rq_begin_started_ns,
                    rq_begin_finished_ns,
                );
                crate::metrics::qperf_record_switch_scheduler_detail(
                    12,
                    rq_begin_finished_ns,
                    rq_preflight_finished_ns,
                );
            }
            return Ok(self.yield_current_rq_owned(cpu.as_mut(), transaction, schedule_out));
        }
        // Preserve requests merged by the rq-owned probe while restoring the
        // full p->pi_lock -> rq order for exceptional task-control work.
        let request = transaction.merge_scheduler_request(SchedulerRequestScope::All);
        transaction.commit();

        self.yield_current_task_control(cpu, previous_core_hint, rq_entry, remote, request)
    }

    /// Handles the uncommon yield path that must serialize task-local state.
    #[cold]
    #[inline(never)]
    fn yield_current_task_control(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        previous_core_hint: &ThreadCore,
        rq_entry: OwnerRqEntry,
        remote: &'static CpuRemote,
        request: SchedulerRequestClaim,
    ) -> Result<YieldOutcome, TaskError> {
        // SAFETY: propagated from the selected entry contract.
        let mut previous_sched = unsafe { rq_entry.lock_thread_sched(previous_core_hint.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, remote) };
        transaction.adopt_scheduler_request(request);
        let now_ns = transaction.clock().wall().as_nanos();
        let dispatch_commit = self.settle_owner_current_dispatch_in_rq(&mut transaction);
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        let previous = transaction.current_thread();
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        let previous_urgency = transaction.current_scheduling_urgency();
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

                let deadline_rq_observation =
                    transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
                transaction.commit_and_finish_scheduler_request();
                drop(previous_sched);
                let endpoint = previous_endpoint.unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1209, core.id().as_u64() as usize)
                });
                let urgency = previous_urgency.unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1209, core.id().as_u64() as usize)
                });
                self.finish_owner_dispatch_commit(dispatch_commit);
                self.finish_owner_selection(
                    cpu.as_mut(),
                    Some(endpoint.thread()),
                    endpoint.thread(),
                    Some(urgency),
                    urgency,
                    OwnerSchedulerDeadline::Reevaluate(deadline_rq_observation),
                );

                return Ok(YieldOutcome::Unchanged);
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
        let OwnerNext {
            core: next_core,
            policy: next_policy,
            urgency: next_urgency,
        } = next;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1210, next_core.as_ref().id().as_u64() as usize)
        });
        let migrated = migration.is_some();
        let handoff = Self::prepare_switch_handoff(
            previous,
            previous_core,
            next_core,
            next_policy,
            PreviousSwitchDisposition::Live,
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
            handoff,
            !migrated && !dispatch_commit.has_deferred_task_lock_work(),
        );
        drop(previous_sched);
        self.finish_owner_dispatch_commit(dispatch_commit);
        self.finish_owner_selection(
            cpu.as_mut(),
            previous_endpoint.map(SwitchEndpoint::thread),
            next_endpoint.thread(),
            previous_urgency,
            next_urgency,
            OwnerSchedulerDeadline::Reevaluate(deadline_rq_observation),
        );
        let decision = Self::owner_switch_plan(previous_endpoint, next_endpoint, reason, now_ns);

        Ok(YieldOutcome::Switch(decision))
    }

    /// Implements Linux's rq-owned ordinary yield path.
    #[inline(never)]
    fn yield_current_rq_owned(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        mut transaction: OwnerRqTxn<'_>,
        schedule_out: OwnerRqScheduleOut,
    ) -> YieldOutcome {
        #[cfg(feature = "qperf-metrics")]
        let qperf_phase_started_ns = task_runtime::monotonic_now().as_nanos();
        let now_ns = transaction.clock().wall().as_nanos();
        let previous = transaction.current_thread();
        let linked_realtime_thread = match &schedule_out {
            OwnerRqScheduleOut::LinkedRealtime { thread } => Some(*thread),
            OwnerRqScheduleOut::Idle { .. } | OwnerRqScheduleOut::Unlinked { .. } => None,
        };
        let dispatch_commit = if linked_realtime_thread.is_some() {
            transaction.settle_fixed_realtime_current();
            OwnerDispatchCommit::NONE
        } else {
            let _settled = transaction.settle_current(0);
            self.sync_owner_settled_current_dispatch_in_rq(&mut transaction)
        };
        transaction.merge_scheduler_request(SchedulerRequestScope::All);
        #[cfg(feature = "qperf-metrics")]
        let qperf_account_finished_ns = task_runtime::monotonic_now().as_nanos();
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::qperf_record_switch_scheduler_detail(
            0,
            qperf_phase_started_ns,
            qperf_account_finished_ns,
        );
        let OwnerRqScheduledOut {
            core: previous_core,
            endpoint: previous_endpoint,
            policy: previous_policy,
            urgency: previous_urgency,
        } = match linked_realtime_thread {
            Some(thread) => self.yield_linked_realtime_owner_rq(&mut transaction, thread),
            None => self.schedule_out_owner_rq_owned(
                &mut transaction,
                schedule_out,
                EnqueueReason::Yield,
            ),
        };
        #[cfg(feature = "qperf-metrics")]
        let qperf_put_prev_finished_ns = task_runtime::monotonic_now().as_nanos();
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::qperf_record_switch_scheduler_detail(
            1,
            qperf_account_finished_ns,
            qperf_put_prev_finished_ns,
        );
        let next = if SchedulerClass::for_policy(previous_policy) == SchedulerClass::Realtime
            && !transaction.rt_is_effectively_throttled()
            && !transaction
                .has_selectable_higher_class(SchedulerClass::Realtime, RtEligibility::Runnable)
        {
            // `yield_task_rt()` has just rotated the retained current node.
            // With the static higher-class prefix proved empty, Linux enters
            // `pick_next_task_rt()` directly and selects that class head.
            self.pick_owner_realtime_after_yield_in_rq(cpu.owner(), &mut transaction)
        } else {
            self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, None)
        };
        let OwnerNext {
            core: next_core,
            policy: next_policy,
            urgency: next_urgency,
        } = next;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1216, next_core.as_ref().id().as_u64() as usize)
        });
        #[cfg(feature = "qperf-metrics")]
        let qperf_pick_finished_ns = task_runtime::monotonic_now().as_nanos();
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::qperf_record_switch_scheduler_detail(
            2,
            qperf_put_prev_finished_ns,
            qperf_pick_finished_ns,
        );
        #[cfg(feature = "qperf-metrics")]
        let qperf_handoff_started_ns = task_runtime::monotonic_now().as_nanos();
        let handoff = Self::prepare_switch_handoff(
            previous,
            Some(previous_core),
            next_core,
            next_policy,
            PreviousSwitchDisposition::Live,
            None,
        );
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::qperf_record_switch_scheduler_detail(
            3,
            qperf_handoff_started_ns,
            task_runtime::monotonic_now().as_nanos(),
        );
        #[cfg(feature = "qperf-metrics")]
        let qperf_rq_commit_started_ns = task_runtime::monotonic_now().as_nanos();
        let scheduler_deadline = if matches!(previous_policy, SchedulePolicy::Fifo { .. })
            && matches!(next_policy, SchedulePolicy::Fifo { .. })
        {
            OwnerSchedulerDeadline::Unchanged
        } else {
            OwnerSchedulerDeadline::Reevaluate(
                transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref()),
            )
        };
        self.commit_owner_switch_selection(
            cpu.as_mut(),
            transaction,
            handoff,
            !dispatch_commit.has_deferred_task_lock_work(),
        );
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::qperf_record_switch_scheduler_detail(
            4,
            qperf_rq_commit_started_ns,
            task_runtime::monotonic_now().as_nanos(),
        );
        #[cfg(feature = "qperf-metrics")]
        let qperf_dispatch_started_ns = task_runtime::monotonic_now().as_nanos();
        self.finish_owner_dispatch_commit(dispatch_commit);
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::qperf_record_switch_scheduler_detail(
            5,
            qperf_dispatch_started_ns,
            task_runtime::monotonic_now().as_nanos(),
        );
        #[cfg(feature = "qperf-metrics")]
        let qperf_selection_tail_started_ns = task_runtime::monotonic_now().as_nanos();
        self.finish_owner_selection(
            cpu.as_mut(),
            Some(previous_endpoint.thread()),
            next_endpoint.thread(),
            Some(previous_urgency),
            next_urgency,
            scheduler_deadline,
        );
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::qperf_record_switch_scheduler_detail(
            6,
            qperf_selection_tail_started_ns,
            task_runtime::monotonic_now().as_nanos(),
        );
        let decision = Self::owner_switch_plan(
            Some(previous_endpoint),
            next_endpoint,
            SwitchReason::Yield,
            now_ns,
        );
        if decision.requires_context_switch() {
            YieldOutcome::Switch(decision)
        } else {
            YieldOutcome::Unchanged
        }
    }
}
