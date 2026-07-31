use super::*;

impl TaskSystem {
    /// Enqueues a ready thread on an affinity-compatible owner CPU.
    pub fn enqueue(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        thread: ThreadId,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let core = {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            Arc::clone(&state.thread_record(thread)?.core)
        };
        self.enqueue_owner_thread(cpu.as_mut(), core, now_ns, EnqueueReason::Wake)?;
        Self::program_local_timer(cpu.as_mut(), now_ns)
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
        now_ns: u64,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let owner = cpu.owner();
        let migration = {
            let state = self.state.lock();
            state.ensure_cpu_online(&cpu)?;
            let record = state.thread_record(thread)?;
            let mut sched = record.sched.lock();
            if sched.lifecycle.state() != ThreadState::Ready {
                return Err(TaskError::NotReady);
            }
            if sched.placement.queued_cpu().is_some()
                || sched.placement.running_cpu().is_some()
                || sched.placement.on_cpu().is_some()
                || sched.placement.migration_target().is_some()
            {
                return Err(TaskError::AlreadyQueued);
            }
            let affinity = &sched.affinity;
            let load_aware = matches!(
                sched.policy,
                SchedulePolicy::Fair {
                    mode: FairMode::Normal | FairMode::Batch,
                    ..
                }
            );
            let target = if load_aware {
                state.select_initial_fair_cpu(affinity, owner)
            } else if affinity.contains(owner) {
                Some(owner)
            } else {
                state.select_allowed_cpu(affinity)
            }
            .ok_or(TaskError::InvalidConfiguration)?;
            let core = Arc::clone(&record.core);
            if target == owner {
                drop(sched);
                drop(state);
                self.enqueue_owner_thread(cpu.as_mut(), core, now_ns, EnqueueReason::Wake)?;
                None
            } else {
                sched.placement.set_migration_target(Some(target))?;
                record.core.set_target_cpu(target);
                drop(sched);
                Some((core, target))
            }
        };
        if let Some((core, target)) = migration {
            return self.publish_owner_migration(&core, target, owner, target);
        }
        Self::program_local_timer(cpu.as_mut(), now_ns)
    }

    /// Removes a ready thread from its owner run queue for migration or update.
    pub fn dequeue(&self, mut cpu: Pin<&mut CpuLocal>, thread: ThreadId) -> Result<(), TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        let state = self.state.lock();
        state.ensure_cpu_online(&cpu)?;
        let queued = cpu
            .as_mut()
            .dispatch_state_mut()
            .run_queue
            .dequeue(thread)
            .ok_or(TaskError::NotReady)?;
        let record = state.thread_record(thread)?;
        let mut sched = record.sched.lock();
        sched.entity = queued.entity;
        if !sched.is_pi_boosted() {
            sched.base_entity = queued.entity;
        }
        sched.placement.set_queued_cpu(None)?;
        drop(sched);
        drop(state);
        self.publish_owner_cpu_load_summary(cpu.as_mut());
        Ok(())
    }

    /// Drains a bounded batch of direct remote wakes on the owner CPU.
    pub fn drain_remote_wakes(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<RemoteWakeDrain, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.ensure_owner_cpu_online(&cpu)?;
        let (drained, pending) = {
            let remote = Arc::clone(cpu.remote());
            let scratch = cpu.as_mut().drain_state_mut();
            let limit = scratch.batch_limit();
            let buffer = &mut scratch.remote_wake_buffer;
            let batch = remote.remote_wake_inbox().drain(limit, buffer);
            (batch.drained(), batch.pending())
        };
        let mut detached = [InboxMessage::EMPTY; crate::DEFAULT_BATCH_LIMIT];
        detached[..drained].copy_from_slice(&cpu.drain_state().remote_wake_buffer[..drained]);
        let mut messages =
            DetachedOwnerMessageBatch::new(&detached[..drained], DetachedPayloadKind::RemoteWake);
        while let Some(message) = messages.next() {
            if message.payload() == 0 {
                continue;
            }
            // SAFETY: ThreadWakeHandle::wake transfers one Arc strong count in
            // every published non-zero payload. This owner drain consumes it
            // exactly once after the intrusive node was detached.
            let core = unsafe {
                Arc::from_raw(ptr::with_exposed_provenance::<ThreadCore>(
                    message.payload(),
                ))
            };
            if core.id() != message.thread_id() {
                continue;
            }
            self.consume_and_place_owner_wake(cpu.as_mut(), core, now_ns)?;
        }
        if pending {
            cpu.request_scheduler_work();
        }
        Ok(RemoteWakeDrain { drained, pending })
    }

    /// Consumes one locally published task-context wake and refreshes the
    /// owner CPU's complete scheduler deadline state.
    ///
    /// The caller must own `cpu` under task-context IRQ exclusion. Hard IRQ
    /// and remote producers instead publish to the lock-free wake inbox and
    /// let the owner consume it at an IRQ-return scheduler safe point.
    pub(crate) fn wake_owner_thread_local(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        if Self::consume_owner_task_wake(&core)? {
            self.place_consumed_owner_wake(cpu.as_mut(), core, now_ns)?;
        }
        Self::program_local_timer(cpu, now_ns)
    }

    fn consume_and_place_owner_wake(
        &self,
        cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        if !Self::consume_owner_wake(&core)? {
            return Ok(());
        }
        self.place_consumed_owner_wake(cpu, core, now_ns)
    }

    fn place_consumed_owner_wake(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let target = core.target_cpu().unwrap_or(owner);
        if target == owner {
            return self.enqueue_owner_thread(cpu, core, now_ns, EnqueueReason::Wake);
        }

        // Affinity may change after a producer selected this CPU. The old
        // owner consumes the lifecycle transition, then hands the ready thread
        // to the latest target instead of placing it on an invalid runqueue.
        Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
        core.sched()
            .lock()
            .placement
            .set_migration_target(Some(target))?;
        self.publish_owner_migration(&core, target, owner, target)
    }

    /// Reconciles task metadata written by a remote affinity setter with the
    /// physical placement owned by this CPU.
    ///
    /// The affinity mask may be updated under the stable thread lock from any
    /// CPU. Runqueue membership and switch-tail state are different: only the
    /// CPU named by [`SchedulerPlacement`] may mutate them. This is the local
    /// equivalent of Linux taking a task's `pi_lock` together with its owning
    /// runqueue lock before moving a queued task.
    fn reconcile_owner_affinity_update(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: &Arc<ThreadCore>,
    ) -> Result<(), TaskError> {
        let owner = cpu.owner();
        let mut sched = core.sched().lock();
        let running_cpu = sched.placement.running_cpu();
        let queued_cpu = sched.placement.queued_cpu();
        let on_cpu = sched.placement.on_cpu();
        let migration_target = sched.placement.migration_target();
        let physical_owner = running_cpu
            .or(queued_cpu)
            .or(on_cpu)
            .or(migration_target)
            .or(sched.deadline_bandwidth_cpu);
        let target = if sched.affinity.contains(owner) {
            owner
        } else {
            self.select_allowed_active_cpu(&sched.affinity, Some(owner))
                .ok_or(TaskError::InvalidConfiguration)?
        };
        core.set_target_cpu(target);

        if let Some(physical_owner) = physical_owner
            && physical_owner != owner
        {
            drop(sched);
            return self.publish_owner_affinity_retry(core, physical_owner, target);
        }

        // A switch handoff owns both the old stack and its committed
        // destination until switch tail clears `on_cpu`. Re-publish the
        // control request rather than rewriting that destination behind the
        // already staged handoff.
        if on_cpu == Some(owner) && running_cpu.is_none() {
            drop(sched);
            self.publish_owner_affinity_retry(core, owner, target)?;
            cpu.request_scheduler_work();
            return Ok(());
        }

        if queued_cpu == Some(owner) {
            if target == owner {
                sched.placement.set_migration_target(None)?;
                let completed = Self::complete_affinity_if_satisfied_locked(core, &sched);
                drop(sched);
                if completed {
                    core.notify_affinity_waiters();
                }
                return Ok(());
            }
            let queued = cpu
                .as_mut()
                .dispatch_state_mut()
                .run_queue
                .dequeue(core.id())
                .ok_or(TaskError::NotReady)?;
            Self::detach_owner_deadline_bandwidth_locked(core, &mut sched, cpu.as_mut())?;
            sched.entity = queued.entity;
            if !sched.is_pi_boosted() {
                sched.base_entity = queued.entity;
            }
            sched.placement.set_migration_target(Some(target))?;
            sched.placement.set_queued_cpu(None)?;
            drop(sched);
            self.publish_owner_cpu_load_summary(cpu.as_mut());
            return self.publish_owner_migration(core, target, owner, target);
        }

        if running_cpu == Some(owner) {
            if cpu.current() != Some(core.id()) {
                return Err(TaskError::InvalidConfiguration);
            }
            sched
                .placement
                .set_migration_target((target != owner).then_some(target))?;
            let completed = Self::complete_affinity_if_satisfied_locked(core, &sched);
            drop(sched);
            if completed {
                core.notify_affinity_waiters();
            }
            if target != owner {
                cpu.request_reschedule();
            }
            self.publish_owner_cpu_load_summary(cpu.as_mut());
            return Ok(());
        }

        if migration_target == Some(owner) {
            if target != owner {
                sched.placement.set_migration_target(Some(target))?;
                drop(sched);
                return self.publish_owner_migration(core, target, owner, target);
            }
            return Ok(());
        }

        if sched.deadline_bandwidth_cpu == Some(owner) && target != owner {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(())
    }

    /// Applies a bounded batch of owner-CPU effective-policy updates.
    pub fn drain_policy_updates(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<RemoteWakeDrain, TaskError> {
        self.ensure_owner_cpu_context(&cpu)?;
        self.ensure_owner_cpu_online(&cpu)?;
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
        let completed_incoming_migrations = detached[..drained]
            .iter()
            .filter(|message| message.operation() == InboxOperation::Migration)
            .count();
        cpu.remote()
            .complete_incoming_migrations(completed_incoming_migrations);
        let mut messages = DetachedOwnerMessageBatch::new(
            &detached[..drained],
            DetachedPayloadKind::SchedulerDelivery,
        );
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
                let target_remote = self
                    .cpu_remotes
                    .get(target.as_usize())
                    .ok_or(TaskError::InvalidCpu(target.as_u32()))?;
                let Some(mut claim) = target_remote.claim_idle_pull(reservation) else {
                    continue;
                };
                if !cpu
                    .try_load_summary()
                    .is_some_and(|summary| summary.is_overloaded())
                {
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
                    now_ns,
                    BalanceReason::IdlePull,
                );
                drop(claim);
                match migrated {
                    Ok(Some(_)) => {}
                    Ok(None) => {
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
                InboxOperation::RemoteWake
                    | InboxOperation::BalanceRequest
                    | InboxOperation::Reclaim
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
                core.sched().lock().placement.set_migration_target(None)?;
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
                if message.generation() <= sched.deadline_cbs_generation {
                    Self::refresh_owner_deadline_timers_locked(&core, &mut sched, cpu.as_mut())?;
                }
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
            if operation == InboxOperation::PolicyUpdate {
                if source != owner || target != owner {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: source.as_u32(),
                        actual: owner.as_u32(),
                    });
                }
                let cleanup_deadline_member = {
                    let sched = core.sched().lock();
                    sched.deadline_cleanup_pending
                        && sched.deadline_bandwidth_cpu == Some(owner)
                        && sched.placement.queued_cpu().is_none()
                        && sched.placement.running_cpu().is_none()
                        && sched.placement.on_cpu().is_none()
                };
                if cleanup_deadline_member {
                    Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                    core.sched().lock().deadline_cleanup_pending = false;
                    continue;
                }
            }
            if operation == InboxOperation::Migration {
                if target != owner {
                    return Err(TaskError::CpuOwnerMismatch {
                        expected: target.as_u32(),
                        actual: owner.as_u32(),
                    });
                }
                let forward_target = {
                    let mut sched = core.sched().lock();
                    let Some(committed_target) = sched.placement.migration_target() else {
                        continue;
                    };
                    let committed_target_accepts_placement = committed_target != owner
                        && sched.affinity.contains(committed_target)
                        && self
                            .cpu_remotes
                            .get(committed_target.as_usize())
                            .is_some_and(|remote| remote.accepts_placement());
                    let latest_target = if sched.affinity.contains(owner)
                        && (committed_target == owner || !committed_target_accepts_placement)
                    {
                        owner
                    } else if committed_target_accepts_placement {
                        committed_target
                    } else {
                        self.select_allowed_active_cpu(&sched.affinity, Some(owner))
                            .ok_or(TaskError::InvalidConfiguration)?
                    };
                    if latest_target != owner {
                        sched.placement.set_migration_target(Some(latest_target))?;
                        core.set_target_cpu(latest_target);
                        Some(latest_target)
                    } else {
                        if sched.lifecycle.state() != ThreadState::Ready
                            || sched.placement.queued_cpu().is_some()
                            || sched.placement.running_cpu().is_some()
                            || sched.placement.on_cpu().is_some()
                        {
                            return Err(TaskError::InvalidConfiguration);
                        }
                        sched.placement.set_migration_target(None)?;
                        core.set_target_cpu(owner);
                        None
                    }
                };
                if let Some(forward_target) = forward_target {
                    self.publish_owner_migration(&core, forward_target, owner, forward_target)?;
                } else {
                    self.enqueue_owner_thread(
                        cpu.as_mut(),
                        Arc::clone(&core),
                        now_ns,
                        EnqueueReason::Migrated,
                    )?;
                }
                continue;
            }
            debug_assert_eq!(operation, InboxOperation::PolicyUpdate);
            let (queued_cpu, running_cpu, policy_generation, cbs_borrowed) = {
                let sched = core.sched().lock();
                (
                    sched.placement.queued_cpu(),
                    sched.placement.running_cpu(),
                    sched.policy_generation,
                    sched.deadline_cbs_borrower.is_some(),
                )
            };
            if message.generation() > policy_generation {
                continue;
            }
            if cbs_borrowed {
                // The remote PI owner is the sole mutable owner of this CBS
                // entity until its next scheduler safe point. Re-publish the
                // cold-path policy update instead of replacing donor state
                // underneath an in-flight dispatch copy.
                self.publish_owner_policy_retry(&core, owner, policy_generation)?;
                cpu.request_scheduler_work();
                continue;
            }
            if queued_cpu == Some(owner) {
                if cpu.dispatch_state().current_dispatch.is_some() {
                    cpu.as_mut().settle_current_dispatch(now_ns, 0)?;
                } else {
                    cpu.as_mut()
                        .dispatch_state_mut()
                        .run_queue
                        .update_fair_virtual_time(None);
                }
                let fair_placement =
                    Self::owner_fair_policy_placement(cpu.as_ref().get_ref(), &core);
                let queued = cpu
                    .as_mut()
                    .dispatch_state_mut()
                    .run_queue
                    .dequeue(core.id())
                    .ok_or(TaskError::NotReady)?;
                Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                {
                    let mut sched = core.sched().lock();
                    if !sched.is_pi_boosted() {
                        sched.base_entity = queued.entity;
                        sched.entity = queued.entity;
                    }
                    sched.placement.set_queued_cpu(None)?;
                }
                let applied = self.apply_owner_policy_generation(
                    &core,
                    message.generation(),
                    now_ns,
                    fair_placement,
                    true,
                )?;
                if applied {
                    self.recompute_pi_after_policy_update(core.id())?;
                }
                self.enqueue_owner_thread(
                    cpu.as_mut(),
                    Arc::clone(&core),
                    now_ns,
                    EnqueueReason::PolicyChanged,
                )?;
                cpu.request_reschedule();
            } else if running_cpu == Some(owner) && cpu.current() == Some(core.id()) {
                self.commit_owner_current_dispatch(cpu.as_mut(), now_ns)?;
                let fair_placement =
                    Self::owner_fair_policy_placement(cpu.as_ref().get_ref(), &core);
                Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                let applied = self.apply_owner_policy_generation(
                    &core,
                    message.generation(),
                    now_ns,
                    fair_placement,
                    true,
                )?;
                if applied {
                    self.recompute_pi_after_policy_update(core.id())?;
                }
                {
                    let mut sched = core.sched().lock();
                    Self::activate_owner_deadline_bandwidth(
                        &core,
                        &mut sched,
                        cpu.as_mut(),
                        owner,
                    )?;
                    let dispatch = Self::owner_dispatch(&core, &sched, now_ns)?;
                    cpu.as_mut().install_dispatch(dispatch);
                }
                self.publish_owner_cpu_load_summary(cpu.as_mut());
                cpu.request_reschedule();
            } else {
                if core.sched().lock().deadline_bandwidth_cpu == Some(owner) {
                    Self::detach_owner_deadline_bandwidth(&core, cpu.as_mut())?;
                }
                let applied = self.apply_owner_policy_generation(
                    &core,
                    message.generation(),
                    now_ns,
                    None,
                    false,
                )?;
                if applied {
                    self.recompute_pi_after_policy_update(core.id())?;
                }
                Self::assign_owner_inactive_deadline_bandwidth(&core, cpu.as_mut())?;
            }
        }
        if pending {
            cpu.request_scheduler_work();
        }
        Ok(RemoteWakeDrain { drained, pending })
    }

    /// Drains one bounded batch from every inbox owned by `cpu`.
    ///
    /// The inboxes, rather than `need_resched`, are the source of truth for
    /// remote scheduler work. Forced scheduling operations call this before
    /// claiming their sticky request so object-API users cannot accidentally
    /// clear a wake, migration, or policy update without first making it
    /// visible to the owner run queue. A bounded remainder is assigned a fresh
    /// runtime doorbell before this safe point returns.
    pub(super) fn drain_owner_work(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
    ) -> Result<(), TaskError> {
        let (wake_pending, policy_pending) = {
            let remote = cpu.remote();
            (
                remote.remote_wake_inbox().has_pending(),
                remote.owner_control_inbox().has_pending(),
            )
        };
        if wake_pending {
            self.drain_remote_wakes(cpu.as_mut(), now_ns)?;
        }
        if policy_pending {
            self.drain_policy_updates(cpu.as_mut(), now_ns)?;
        }
        if cpu.has_remote_work() {
            cpu.defer_scheduler_work();
        }
        Ok(())
    }
}
