//! Owner scheduling entry points, runtime charging, and load balancing requests.

use super::*;

impl TaskSystem {
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
            .find_idle_pull_source(target, cpu.idle_pull_visited())
            .or_else(|| {
                self.cpu_remotes
                    .iter()
                    .enumerate()
                    .filter(|(index, remote)| {
                        let source = CpuId::new(*index as u32);
                        remote.accepts_placement()
                            && source != target
                            && !cpu.idle_pull_visited().contains(source)
                    })
                    .filter_map(|(index, local)| {
                        let source = CpuId::new(index as u32);
                        let summary = local.load_summary();
                        summary
                            .has_pushable_fair()
                            .then_some((summary.fair_demand(), source))
                    })
                    .max_by_key(|(demand, source)| (*demand, core::cmp::Reverse(source.as_u32())))
                    .map(|(_, source)| (source, SchedulingClass::Fair))
            });
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
        self.schedule_owner(cpu, current, OwnerRqEntry::IrqSave)
    }

    fn schedule_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: Option<&ThreadHandle>,
        rq_entry: OwnerRqEntry,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let remote = Arc::clone(cpu.remote());
        let initial_request = remote.claim_scheduler_request();
        // SAFETY: propagated from the selected entry contract.
        unsafe { self.complete_context_switch_owner(cpu.as_mut(), rq_entry)? };
        self.drain_owner_work(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let previous_core_hint = current.map(|thread| Arc::clone(thread.runtime_core_arc()));
        let mut previous_sched = previous_core_hint.as_ref().map(|core| {
            // SAFETY: propagated from the selected entry contract.
            unsafe { rq_entry.lock_thread_sched(core.sched()) }
        });
        // SAFETY: the public task entry chooses irqsave; the scheduler-frame
        // entry is exposed only by its unsafe wrapper below.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        transaction.adopt_scheduler_request(initial_request);
        transaction.merge_scheduler_request();
        let dispatch_commit = self.commit_owner_current_dispatch_in_rq(&mut transaction);
        // Runtime accounting is part of this unconditional scheduling
        // decision, exactly like Linux update_curr() preceding pick_next.
        transaction.merge_scheduler_request();
        let previous = transaction.current_thread();
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        if previous_core.as_ref().map(Arc::as_ptr) != previous_core_hint.as_ref().map(Arc::as_ptr) {
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
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, previous);
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
        transaction.commit_and_acknowledge_scheduler_request();
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
        self.schedule_if_requested_owner(cpu, current, OwnerRqEntry::IrqSave)
    }

    /// Services scheduler work while the runtime owns the IRQ-off baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn schedule_if_requested_in_scheduler_frame(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<SchedulerOutcome, TaskError> {
        self.schedule_if_requested_owner(cpu, current, OwnerRqEntry::SchedulerFrame)
    }

    fn schedule_if_requested_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
        rq_entry: OwnerRqEntry,
    ) -> Result<SchedulerOutcome, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let remote = Arc::clone(cpu.remote());
        let initial_request = remote.claim_scheduler_request();
        // SAFETY: propagated from the selected entry contract.
        unsafe { self.complete_context_switch_owner(cpu.as_mut(), rq_entry)? };
        self.drain_owner_work(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let previous_core_hint = Arc::clone(current.runtime_core_arc());
        // SAFETY: propagated from the selected entry contract.
        let mut previous_sched = unsafe { rq_entry.lock_thread_sched(previous_core_hint.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        transaction.adopt_scheduler_request(initial_request);
        if transaction.current().is_some() {
            let _settled = transaction.settle_current(0);
        }
        // This claim is the decision boundary: requests published by current
        // accounting participate in this pass; later generations do not.
        let request = transaction.merge_scheduler_request();
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
            cpu.defer_park_preemption(request.preempt_requested());
            transaction.commit_and_acknowledge_scheduler_request();
            return Ok(SchedulerOutcome::ParkingDeferred);
        }
        let switch_requested = request.preempt_requested();
        let previous = transaction.current_thread();
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        if previous_core
            .as_ref()
            .is_none_or(|core| !Arc::ptr_eq(core, &previous_core_hint))
        {
            task_runtime::fatal_invariant(0x5343_1204, cpu.owner().as_u32() as usize);
        }
        if previous_core.is_some() && !switch_requested {
            let runtime_overrun_work = self.sync_owner_current_dispatch_in_rq(&mut transaction);
            let deadline_rq_observation =
                transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
            transaction.commit_and_acknowledge_scheduler_request();
            drop(previous_sched);
            if let Some(core) = runtime_overrun_work {
                self.publish_deadline_overrun_work(core);
            }
            let current = previous.expect("a current core must retain its thread identity");
            let run_queue_changed =
                if self.owner_balance_work_pending(cpu.as_ref().get_ref(), current) {
                    self.service_owner_balance(cpu.as_mut(), current)?
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
            return Ok(if cpu.needs_reschedule() || cpu.has_remote_work() {
                SchedulerOutcome::OwnerWorkPending
            } else {
                SchedulerOutcome::Quiescent
            });
        }
        let dispatch_commit = self.commit_owner_settled_current_dispatch_in_rq(&mut transaction);
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
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, previous);
        let next_core = next.core;
        let next_endpoint = transaction.current_switch_endpoint().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1206, next_core.id().as_u64() as usize)
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
        transaction.commit_and_acknowledge_scheduler_request();
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
        Ok(SchedulerOutcome::Decision(decision))
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
        self.yield_current_owner(cpu, current, OwnerRqEntry::IrqSave)
    }

    /// Yields while the runtime owns the IRQ-off scheduler baton.
    ///
    /// # Safety
    ///
    /// The scheduler frame must remain active until this function returns.
    pub(crate) unsafe fn yield_current_in_scheduler_frame(
        &self,
        cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
    ) -> Result<ScheduleDecision, TaskError> {
        self.yield_current_owner(cpu, current, OwnerRqEntry::SchedulerFrame)
    }

    fn yield_current_owner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        current: &ThreadHandle,
        rq_entry: OwnerRqEntry,
    ) -> Result<ScheduleDecision, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let remote = Arc::clone(cpu.remote());
        let initial_request = remote.claim_scheduler_request();
        // SAFETY: propagated from the selected entry contract.
        unsafe { self.complete_context_switch_owner(cpu.as_mut(), rq_entry)? };
        self.drain_owner_work(cpu.as_mut())?;
        self.ensure_owner_cpu_online(&cpu)?;
        let previous_core_hint = Arc::clone(current.runtime_core_arc());
        // SAFETY: propagated from the selected entry contract.
        let mut previous_sched = unsafe { rq_entry.lock_thread_sched(previous_core_hint.sched()) };
        // SAFETY: propagated from the selected entry contract.
        let mut transaction = unsafe { rq_entry.begin(self, &remote) };
        let clock = transaction.clock();
        let now_ns = clock.wall().as_nanos();
        transaction.adopt_scheduler_request(initial_request);
        transaction.merge_scheduler_request();
        let dispatch_commit = self.commit_owner_current_dispatch_in_rq(&mut transaction);
        // A forced yield consumes a slice-expiration request discovered while
        // accounting the outgoing task in this same scheduling pass.
        transaction.merge_scheduler_request();
        let previous = transaction.current_thread();
        let previous_core = transaction.current_core();
        let previous_endpoint = transaction.current_switch_endpoint();
        if previous_core
            .as_ref()
            .is_none_or(|core| !Arc::ptr_eq(core, &previous_core_hint))
        {
            task_runtime::fatal_invariant(0x5343_1207, cpu.owner().as_u32() as usize);
        }
        if let Some(core) = previous_core.as_ref() {
            let owner = cpu.owner();
            let continuing_dispatch = {
                matches!(
                    transaction.current_scheduling_entity(),
                    Some(SchedulingEntity::Fair(_))
                ) && core.sched().placement().can_continue_running_on(owner)
                    && previous_sched.affinity.affinity.contains(owner)
                    && transaction.nr_queued() == 0
            };
            if continuing_dispatch {
                // Linux `yield_task_fair()` returns before changing the active
                // EEVDF request when this is the only runnable entity. Moving
                // the owner through Ready and the runqueue here would
                // forfeit its request even though no peer could consume the
                // yielded service.
                let deadline_rq_observation =
                    transaction.scheduler_deadline_rq_observation(cpu.as_ref().get_ref());
                transaction.commit_and_acknowledge_scheduler_request();
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
                        || placement.execution_cpu() != Some(cpu.owner())
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
                    sched
                        .transition(core, ThreadState::Ready)
                        .unwrap_or_else(|_| {
                            task_runtime::fatal_invariant(0x5343_120e, core.id().as_u64() as usize)
                        });
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
        let next = self.pick_owner_next_in_rq(cpu.as_mut(), &mut transaction, previous);
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
        transaction.commit_and_acknowledge_scheduler_request();
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
}
