use super::{
    dispatch::{PolicyApplication, PolicyGenerationCommit},
    *,
};
use crate::system::OwnerRqTaskState;

pub(super) struct OwnerPolicyApply {
    pub(super) commit: PolicyGenerationCommit,
    pub(super) preempts_current: bool,
    pub(super) scheduler_deadline_refresh_required: bool,
    pub(super) rt_period_started: bool,
    pub(super) effective_policy: SchedulePolicy,
    pub(super) effective_entity: SchedulingEntity,
}

impl TaskSystem {
    pub(super) fn apply_owner_policy_update_locked(
        &self,
        remote: &CpuRemote,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        generation: u64,
    ) -> Result<OwnerPolicyApply, TaskError> {
        let owner = remote.owner();
        let donor = sched.pi.donors.first_entry();
        Self::validate_owner_policy_generation(sched, generation)?;
        let mut transaction = OwnerRqTxn::begin(self, remote);
        let owner_now_ns = transaction.clock().wall().as_nanos();
        if transaction.current().is_some() {
            let _settled = transaction.settle_current(0);
        }
        let rq_state = transaction.task_state(core.id(), &sched.placement);
        let destination_mode = match sched.policy.requested_policy() {
            SchedulePolicy::Fair { mode, .. } => Some(mode),
            _ => None,
        };
        let fair_placement = destination_mode.map(|destination_mode| {
            let source_entity = sched
                .policy
                .active_option()
                .map(|active| active.base_entity().clone())
                .or_else(|| transaction.base_scheduling_entity(core.id()))
                .unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5251_1202, core.id().as_u64() as usize)
                });
            let source_mode = source_entity
                .fair()
                .map_or(destination_mode, |fair| fair.mode());
            FairPolicyPlacement {
                source_virtual_time: transaction.virtual_time_for_mode(source_mode),
                destination_virtual_time: transaction.virtual_time_for_mode(destination_mode),
            }
        });
        let mut active = match rq_state {
            OwnerRqTaskState::Current => transaction.detach_current_schedule(core.id()),
            OwnerRqTaskState::Queued { outgoing } => {
                let detached = transaction.reclassify_task(core.id());
                if !outgoing {
                    sched.placement.deactivate(owner);
                }
                detached.into_active()
            }
            OwnerRqTaskState::Inactive => sched.policy.take_active(),
        };
        Self::detach_owner_deadline_bandwidth_in_rq(core, sched, remote, &mut transaction);
        let commit = self
            .apply_policy_generation_locked(
                sched,
                &mut active,
                generation,
                owner_now_ns,
                fair_placement,
                PolicyApplication::from_rq_state(rq_state, owner_now_ns),
            )
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1203, core.id().as_u64() as usize)
            })
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5251_1204, core.id().as_u64() as usize)
            });
        let base_entity = active.base_entity().clone();
        let pi_update = self
            .resolved_pi_schedule_update(
                sched.policy.base,
                base_entity,
                donor,
                sched.policy.dispatch_generation,
            )
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1206, core.id().as_u64() as usize)
            });
        active = apply_pi_schedule_update(sched, active, pi_update, owner_now_ns, fair_placement)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5251_1207, core.id().as_u64() as usize)
            });
        let effective_policy = active.policy();
        let effective_entity = active.entity().clone();
        let enqueue = match rq_state {
            OwnerRqTaskState::Current => {
                Self::activate_deadline_bandwidth_locked(core, sched, &mut transaction, owner);
                let rt_quota_exempt = sched.is_pi_boosted_rt_owner_for(active.policy());
                let migration_capable = sched.affinity.affinity.is_migration_capable();
                let metadata = sched.rq_task_metadata().unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5251_1205, core.id().as_u64() as usize)
                });
                transaction.install_current_schedule(
                    core.id(),
                    active,
                    Arc::clone(core),
                    rt_quota_exempt,
                    migration_capable,
                    metadata.clone(),
                );
                transaction.refresh_current_scheduler_metadata(
                    core.id(),
                    metadata,
                    rt_quota_exempt,
                );
                dispatch::OwnerReadyEnqueue {
                    preempts_current: true,
                    scheduler_deadline_refresh_required: false,
                }
            }
            OwnerRqTaskState::Queued { .. } => {
                sched.policy.install_active(active);
                self.link_owner_ready_thread_locked(
                    owner,
                    &mut transaction,
                    core,
                    sched,
                    EnqueueReason::PolicyChanged,
                )
            }
            OwnerRqTaskState::Inactive => {
                sched.policy.install_active(active);
                dispatch::OwnerReadyEnqueue {
                    preempts_current: false,
                    scheduler_deadline_refresh_required: false,
                }
            }
        };
        transaction.commit();
        // Linux starts rt_bandwidth when sched_setscheduler() re-enqueues an
        // RT entity. Current tasks are detached and reinstalled rather than
        // passing through the ordinary wake/enqueue completion path, so the
        // policy transaction owns the equivalent activation edge.
        let rt_period_started = rq_state.is_runnable()
            && self.activate_owner_rt_period_for_policy(owner, effective_policy);
        Ok(OwnerPolicyApply {
            commit,
            preempts_current: enqueue.preempts_current,
            scheduler_deadline_refresh_required: enqueue.scheduler_deadline_refresh_required,
            rt_period_started,
            effective_policy,
            effective_entity,
        })
    }

    /// Enqueues a ready thread on an affinity-compatible owner CPU.
    pub fn enqueue(&self, mut cpu: Pin<&mut CpuLocal>, thread: ThreadId) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let core = {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            Arc::clone(&state.thread_record(thread)?.core)
        };
        self.enqueue_owner_thread(cpu.as_mut(), core, EnqueueReason::Wake)?;
        self.program_local_timer(cpu.as_mut(), SchedulerDeadlineDerivationSource::Enqueue)
    }

    /// Places a newly ready thread on an allowed active CPU.
    ///
    /// Ordinary fair work is placed on the least-loaded allowed CPU, including
    /// its current non-idle dispatch and migrations not yet consumed by the
    /// destination owner. Other classes preserve owner-local placement unless
    /// affinity requires a transfer. Remote placement uses the owner-only
    /// owner-control inbox and never mutates another CPU's runqueue.
    ///
    /// # Errors
    ///
    /// Returns an error when the source CPU is offline, the thread is not a
    /// unique unqueued Ready thread, no allowed CPU is online, or local timer
    /// programming fails.
    pub fn place_ready(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let owner = cpu.owner();
        let migration = {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let record = state.thread_record(thread)?;
            let sched = record.sched.lock();
            if sched.lifecycle.state() != ThreadState::Ready {
                return Err(TaskError::NotReady);
            }
            if sched.placement.queued_cpu().is_some()
                || sched.placement.on_cpu().is_some()
                || sched.placement.has_pending_migration()
            {
                return Err(TaskError::AlreadyQueued);
            }
            let affinity = &sched.affinity.affinity;
            let policy = sched.policy.active().policy();
            let load_aware = matches!(
                policy,
                SchedulePolicy::Fair {
                    mode: FairMode::Normal | FairMode::Batch,
                    ..
                }
            );
            let target = if load_aware {
                state.select_initial_fair_cpu(affinity, Some(owner))
            } else if matches!(
                policy,
                SchedulePolicy::Fifo { .. }
                    | SchedulePolicy::RoundRobin { .. }
                    | SchedulePolicy::Deadline(_)
            ) {
                self.select_priority_cpu(
                    policy,
                    sched.policy.active().entity(),
                    affinity,
                    Some(owner),
                    None,
                )
            } else if affinity.contains(owner) {
                Some(owner)
            } else {
                self.select_fallback_active_cpu(affinity, None)
            }
            .ok_or(TaskError::InvalidConfiguration)?;
            let core = Arc::clone(&record.core);
            if target == owner {
                drop(sched);
                drop(state);
                self.enqueue_owner_thread(cpu.as_mut(), core, EnqueueReason::Wake)?;
                None
            } else {
                let carrier = self.prepare_owner_migration(&core, owner, target)?;
                sched.placement.begin_remote_wakeup(target);
                record.core.set_wake_cpu_hint(target);
                drop(sched);
                Some((carrier, target))
            }
        };
        if let Some((carrier, target)) = migration {
            #[cfg(not(any(test, all(axtest, feature = "axtest"))))]
            let _ = target;
            #[cfg(any(test, all(axtest, feature = "axtest")))]
            placement::inject_migration_publication_race(self, target);
            carrier.commit();
            return Ok(());
        }
        self.program_local_timer(cpu.as_mut(), SchedulerDeadlineDerivationSource::Placement)
    }

    /// Removes a ready thread from its owner run queue for migration or update.
    pub fn dequeue(&self, cpu: Pin<&mut CpuLocal>, thread: ThreadId) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let record = state.thread_record(thread)?;
        let mut sched = record.sched.lock();
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        if transaction.current_thread() == Some(thread)
            || transaction.scheduling_entity(thread).is_none()
        {
            transaction.commit();
            return Err(TaskError::NotReady);
        }
        let queued = transaction.deactivate_task(thread);
        sched.policy.install_active(queued.into_active());
        sched.placement.deactivate(cpu.owner());
        transaction.commit();
        drop(sched);
        drop(state);
        Ok(())
    }

    /// Reconciles task metadata written by a remote affinity setter with the
    /// physical placement owned by this CPU.
    ///
    /// The affinity mask may be updated under the stable thread lock from any
    /// CPU. Runqueue membership and switch-tail state are different: only the
    /// CPU named by the placement state may mutate them. This is the local
    /// equivalent of Linux taking a task's `pi_lock` together with its owning
    /// runqueue lock before moving a queued task.
    fn reconcile_owner_affinity_update(
        &self,
        cpu: Pin<&mut CpuLocal>,
        core: &Arc<ThreadCore>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        let queued_cpu = sched.placement.queued_cpu();
        let on_cpu = sched.placement.on_cpu();
        let migration_target = sched.placement.committed_migration_target();
        let physical_owner = sched.placement.control_owner();
        let target = if sched.affinity.affinity.contains(owner) {
            owner
        } else {
            let (policy, entity) = self.affinity_schedule_state_locked(core, &sched)?;
            self.select_priority_cpu(policy, &entity, &sched.affinity.affinity, None, Some(owner))
                .ok_or(TaskError::InvalidConfiguration)?
        };
        core.set_wake_cpu_hint(target);

        if let Some(physical_owner) = physical_owner
            && physical_owner != owner
        {
            drop(sched);
            return self.publish_owner_affinity_retry(core, physical_owner, target);
        }

        // Owner-control draining is forbidden while a switch handoff exists,
        // so an outgoing-only `on_cpu` owner here indicates corrupt placement
        // state rather than work that can be made safe by self-republication.
        if on_cpu == Some(owner) && cpu.current() != Some(core.id()) {
            return Err(TaskError::InvalidConfiguration);
        }

        if queued_cpu == Some(owner) && on_cpu.is_none() {
            let remote = Arc::clone(cpu.remote());
            let carrier = (target != owner)
                .then(|| self.prepare_owner_migration(core, owner, target))
                .transpose()?;
            let mut transaction = OwnerRqTxn::begin(self, &remote);
            transaction.update_migration_capability(
                core.id(),
                sched.affinity.affinity.is_migration_capable(),
            );
            if target == owner {
                sched.placement.request_migration(None);
                let completed = Self::complete_affinity_if_satisfied_locked(core, &sched);
                transaction.commit();
                drop(sched);
                if completed {
                    core.notify_affinity_waiters();
                }
                return Ok(());
            }
            let detached = {
                let current_fair = transaction
                    .current_scheduling_entity()
                    .and_then(|entity| entity.fair());
                let detached = transaction.detach_for_transfer(
                    core.id(),
                    current_fair,
                    self.config.timing_granularity_ns(),
                );
                let Some(detached) = detached else {
                    transaction.commit();
                    return Err(TaskError::NotReady);
                };
                detached
            };
            Self::detach_owner_deadline_bandwidth_in_rq(
                core,
                &mut sched,
                cpu.remote(),
                &mut transaction,
            );
            sched.policy.install_active(detached.into_active());
            sched.placement.begin_migration(owner, target);
            core.set_wake_cpu_hint(target);
            transaction.commit();
            drop(sched);
            carrier
                .expect("a remote affinity target must reserve one migration carrier")
                .commit();
            return Ok(());
        }

        if on_cpu == Some(owner) {
            sched
                .placement
                .request_migration((target != owner).then_some(target));
            let completed = Self::complete_affinity_if_satisfied_locked(core, &sched);
            drop(sched);
            if completed {
                core.notify_affinity_waiters();
            }
            if target != owner {
                cpu.request_reschedule();
            }
            return Ok(());
        }

        if migration_target == Some(owner) {
            sched
                .placement
                .request_migration((target != owner).then_some(target));
            return Ok(());
        }

        let completed = Self::complete_affinity_if_satisfied_locked(core, &sched);
        drop(sched);
        if completed {
            core.notify_affinity_waiters();
        }
        Ok(())
    }

    /// Applies a bounded batch of owner-CPU effective-policy updates.
    pub fn drain_owner_control(
        &self,
        cpu: Pin<&mut CpuLocal>,
    ) -> Result<OwnerControlDrain, TaskError> {
        self.drain_owner_control_inner(cpu)
    }

    fn drain_owner_control_inner(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<OwnerControlDrain, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.ensure_owner_cpu_online(&cpu)?;
        // Owner-control work is ordered after the architecture switch tail.
        // Until then `on_cpu` is a lifetime pin for the outgoing stack, not a
        // runnable-placement owner. Consuming an affinity update in this
        // window either has to republish itself indefinitely or can lose the
        // completion when the tail detaches a blocked task. Linux closes the
        // same interval in `finish_task_switch()` before the rq owner handles
        // migration work. Keep the original intrusive publication pending and
        // make the scheduler revisit it after tail instead.
        if cpu.as_ref().get_ref().switch_handoff().is_some()
            && cpu.remote().owner_control_inbox().has_pending()
        {
            cpu.request_scheduler_work();
            return Ok(OwnerControlDrain {
                drained: 0,
                pending: true,
            });
        }
        let (drained, pending) = {
            let remote = Arc::clone(cpu.remote());
            let scratch = cpu.as_mut().drain_state_mut();
            let limit = scratch.batch_limit();
            let batch = remote
                .owner_control_inbox()
                .drain(limit, &mut scratch.owner_control_buffer);
            (batch.drained(), batch.pending())
        };
        let mut detached = [InboxMessage::EMPTY; crate::DEFAULT_BATCH_LIMIT];
        detached[..drained].copy_from_slice(&cpu.drain_state().owner_control_buffer[..drained]);
        let completed_incoming_migration_demand = detached[..drained]
            .iter()
            .filter(|message| message.operation() == InboxOperation::Migration)
            .try_fold(0_u64, |demand, message| {
                demand.checked_add(message.placement_demand())
            })
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x4d49_4744, cpu.owner().as_u32() as usize)
            });
        cpu.remote()
            .release_incoming_migration_demand(completed_incoming_migration_demand);
        let mut messages = DetachedOwnerMessageBatch::new(&detached[..drained]);
        while let Some(message) = messages.next() {
            let operation = message.operation();
            if operation == InboxOperation::BalanceRequest {
                let source = message
                    .source_cpu()
                    .ok_or(TaskError::InvalidConfiguration)?;
                let target = message
                    .target_cpu()
                    .ok_or(TaskError::InvalidConfiguration)?;
                if source != cpu.owner() {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: source.as_u32(),
                        actual: cpu.owner().as_u32(),
                    });
                }
                let reservation = message
                    .balance_reservation()
                    .ok_or(TaskError::InvalidConfiguration)?;
                let balance_class = message
                    .balance_class()
                    .ok_or(TaskError::InvalidConfiguration)?;
                let target_remote = self
                    .cpu_remotes
                    .get(target.as_usize())
                    .ok_or(TaskError::InvalidCpu(target.as_u32()))?;
                let Some(mut claim) = target_remote.claim_idle_pull(reservation) else {
                    continue;
                };
                let source_has_candidate = match balance_class {
                    SchedulingClass::Deadline | SchedulingClass::Realtime => self
                        .root_domain
                        .cpu_has_overload(cpu.owner(), balance_class),
                    SchedulingClass::Fair => cpu.load_summary().has_pushable_fair(),
                    SchedulingClass::Stop | SchedulingClass::Idle => false,
                };
                if !source_has_candidate {
                    drop(claim);
                    target_remote.kick_scheduler_work();
                    continue;
                }
                if !claim.commit() {
                    continue;
                }
                let migrated = self.transfer_owner_balance_candidate(
                    cpu.as_mut(),
                    target,
                    BalanceReason::IdlePull,
                    Some(balance_class),
                );
                drop(claim);
                match migrated {
                    Ok(BalanceTransferOutcome::Migrated(_)) => {}
                    Ok(BalanceTransferOutcome::NoCandidate | BalanceTransferOutcome::Retry) => {
                        target_remote.kick_scheduler_work();
                    }
                    Err(error) => {
                        target_remote.kick_scheduler_work();
                        return Err(error);
                    }
                }
                continue;
            }
            if matches!(
                operation,
                InboxOperation::BalanceRequest | InboxOperation::Reclaim
            ) {
                return Err(TaskError::InvalidConfiguration);
            }
            if message.payload() == 0 {
                continue;
            }
            // SAFETY: publication transfers one Arc count in the payload and
            // this detached owner message consumes that count exactly once.
            let core = unsafe {
                Arc::from_raw(ptr::with_exposed_provenance::<ThreadCore>(
                    message.payload(),
                ))
            };
            let _delivery = core.accept_scheduler_inbox_delivery();
            if core.id() != message.thread_id() {
                continue;
            }
            let Some(_activity) = core.try_scheduler_activity() else {
                // Exit owns the transition gate and will clear any pending
                // migration target before publishing the reaper retry.
                continue;
            };
            if core.state() == ThreadState::Exited {
                continue;
            }
            let owner = cpu.owner();
            let source = message
                .source_cpu()
                .ok_or(TaskError::InvalidConfiguration)?;
            let target = message
                .target_cpu()
                .ok_or(TaskError::InvalidConfiguration)?;
            if operation == InboxOperation::DeadlineRefresh {
                if source != owner || target != owner {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: source.as_u32(),
                        actual: owner.as_u32(),
                    });
                }
                let mut sched = core.sched().lock();
                if sched.placement.queued_cpu() == Some(owner) {
                    self.activate_owner_deadline_bandwidth(&core, &mut sched, cpu.as_mut(), owner);
                }
                self.refresh_owner_deadline_timers_locked(&core, &mut sched, cpu.as_mut());
                continue;
            }
            if operation == InboxOperation::AffinityUpdate {
                if source != owner {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: source.as_u32(),
                        actual: owner.as_u32(),
                    });
                }
                self.reconcile_owner_affinity_update(cpu.as_mut(), &core)?;
                continue;
            }
            if operation == InboxOperation::Migration {
                if target != owner {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: target.as_u32(),
                        actual: owner.as_u32(),
                    });
                }
                let needs_affinity_move = {
                    let sched = core.sched().lock();
                    if sched.lifecycle.state() != ThreadState::Ready
                        || sched.placement.committed_migration_target() != Some(owner)
                        || sched.placement.queued_cpu().is_some()
                        || sched.placement.on_cpu().is_some()
                    {
                        return Err(TaskError::InvalidConfiguration);
                    }
                    !sched.affinity.affinity.contains(owner)
                        || sched.placement.requested_migration().is_some()
                };
                self.enqueue_owner_thread(
                    cpu.as_mut(),
                    Arc::clone(&core),
                    EnqueueReason::Migrated,
                )?;
                if needs_affinity_move {
                    self.reconcile_owner_affinity_update(cpu.as_mut(), &core)?;
                }
                continue;
            }
            return Err(TaskError::InvalidConfiguration);
        }
        if pending {
            cpu.request_scheduler_work();
        }
        Ok(OwnerControlDrain { drained, pending })
    }

    /// Drains one bounded batch from every inbox owned by `cpu`.
    ///
    /// Owner-control inboxes, rather than `need_resched`, are the source of
    /// truth for migration, policy, and deferred owner work. Direct wakeups
    /// have already activated the target runqueue before this safe point. A
    /// bounded owner-work remainder is assigned a fresh runtime doorbell before
    /// this safe point returns.
    pub(super) fn drain_owner_work(&self, mut cpu: Pin<&mut CpuLocal>) -> Result<(), TaskError> {
        if cpu.as_mut().begin_hard_timer_work() {
            let now = task_runtime::monotonic_now();
            let budget = cpu.batch_limit();
            let service = self.service_due_scheduler_deadlines(cpu.as_mut(), now, budget);
            let pending = service.as_ref().copied().unwrap_or(true);
            cpu.as_mut().finish_hard_timer_work(pending);
            service?;
        }
        let policy_pending = cpu.remote().owner_control_inbox().has_pending();
        if policy_pending {
            self.drain_owner_control_inner(cpu.as_mut())?;
        }
        if cpu.has_remote_work() {
            cpu.defer_scheduler_work();
        }
        Ok(())
    }
}
