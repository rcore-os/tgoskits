//! Control under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Applies a bounded batch of owner-CPU effective-policy updates.
    pub fn drain_owner_control(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<OwnerControlDrain, TaskError> {
        let drained = self.drain_owner_control_inner(cpu.as_mut())?;
        if drained.pending {
            // This standalone PI safe point has no scheduler transaction whose
            // final recheck can rearm the detached bounded remainder.
            cpu.defer_scheduler_work();
        }
        Ok(drained)
    }

    pub(super) fn drain_owner_control_inner(
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
        let mut detached = [InboxMessage::EMPTY; crate::runtime::config::DEFAULT_BATCH_LIMIT];
        detached[..drained].copy_from_slice(&cpu.drain_state().owner_control_buffer[..drained]);
        // An incoming migration remains visible to placement readers until the
        // owner has processed the complete detached batch. Releasing before
        // enqueue creates a false-idle window in which another waker can stack
        // work on this CPU.
        let completed_incoming_migration_demand = detached[..drained]
            .iter()
            .filter(|message| message.operation() == InboxOperation::Migration)
            .try_fold(0_u64, |demand, message| {
                demand.checked_add(message.placement_demand())
            })
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x4d49_4744, cpu.owner().as_u32() as usize)
            });
        let _incoming_migration = IncomingMigrationBatch::new(
            Arc::clone(cpu.remote()),
            completed_incoming_migration_demand,
        );
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
                    SchedulingClass::Stop => false,
                };
                if !source_has_candidate {
                    drop(claim);
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
                    // Linux `sched_balance_newidle()` ends this newly-idle
                    // pass when the selected source cannot detach a task. A
                    // later idle entry or periodic balance supplies the next
                    // independent attempt; the target does not kick itself.
                    Ok(BalanceTransferOutcome::NoCandidate) => {}
                    Ok(BalanceTransferOutcome::Retry) => {}
                    Err(error) => {
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
                let mut sched = core.sched().lock();
                let committed_here = sched.placement.committed_migration_target() == Some(owner)
                    && sched.placement.queued_cpu().is_none()
                    && sched.placement.on_cpu().is_none();
                let delayed_migration = sched.lifecycle.state() == ThreadState::Blocked
                    && committed_here
                    && core
                        .sched()
                        .active_option(&sched)
                        .and_then(|active| active.entity().fair())
                        .is_some_and(|fair| fair.is_delayed_migrating());
                if delayed_migration {
                    let needs_affinity_move = !sched.affinity.affinity.contains(owner)
                        || sched.placement.requested_migration().is_some();
                    cpu.remote().cancel_idle_pull_if_uncommitted();
                    let remote = Arc::clone(cpu.remote());
                    let mut transaction = OwnerRqTxn::begin(self, &remote);
                    let current_fair = transaction.current_fair_contender();
                    transaction.update_fair_virtual_time(current_fair);
                    let policy = core.sched().active(&sched).policy();
                    let metadata = sched.rq_task_metadata()?;
                    let rt_quota_exempt = sched.is_pi_boosted_rt_owner_for(policy);
                    let active = core.sched().take_active(&mut sched);
                    let enqueue = transaction.enqueue_delayed_fair_transfer(
                        QueuedThread::new(
                            core.id(),
                            active,
                            Arc::clone(&core),
                            rt_quota_exempt,
                            sched.affinity.affinity.is_migration_capable(),
                            metadata,
                        ),
                        current_fair,
                    );
                    transaction.update_fair_virtual_time(current_fair);
                    sched.placement.activate(owner);
                    core.publish_effective_schedule(policy, enqueue.entity());
                    core.set_wake_cpu_hint(owner);
                    let affinity_completed =
                        Self::complete_affinity_if_satisfied_locked(&core, &sched);
                    let scheduler_deadline_refresh_required =
                        enqueue.scheduler_deadline_refresh_required();
                    transaction.commit();
                    drop(sched);
                    if affinity_completed {
                        core.notify_affinity_waiters();
                    }
                    if needs_affinity_move {
                        self.reconcile_owner_affinity_update(cpu.as_mut(), &core)?;
                    } else if scheduler_deadline_refresh_required {
                        remote.kick_scheduler_work();
                    }
                    continue;
                }

                // A direct wake may win the task lock after the source commits
                // its carrier but before this owner consumes it. In that case
                // wake has already activated the exact committed destination;
                // consume this now-stale transport and finish affinity work.
                if sched.lifecycle.state() == ThreadState::Running
                    && !committed_here
                    && sched.placement.committed_migration_target().is_none()
                    && (sched.placement.queued_cpu() == Some(owner)
                        || sched.placement.on_cpu() == Some(owner))
                {
                    let needs_affinity_move = !sched.affinity.affinity.contains(owner)
                        || sched.placement.requested_migration().is_some();
                    let affinity_completed =
                        Self::complete_affinity_if_satisfied_locked(&core, &sched);
                    drop(sched);
                    if affinity_completed {
                        core.notify_affinity_waiters();
                    }
                    if needs_affinity_move {
                        self.reconcile_owner_affinity_update(cpu.as_mut(), &core)?;
                    }
                    continue;
                }

                if sched.lifecycle.state() != ThreadState::Running || !committed_here {
                    return Err(TaskError::InvalidConfiguration);
                }
                let needs_affinity_move = !sched.affinity.affinity.contains(owner)
                    || sched.placement.requested_migration().is_some();
                drop(sched);
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
        Ok(OwnerControlDrain { drained, pending })
    }

    /// Drains one bounded batch from every inbox owned by `cpu`.
    ///
    /// Owner-control inboxes, rather than `need_resched`, are the source of
    /// truth for migration, policy, and deferred owner work. A bounded
    /// owner-work remainder is rearmed by the scheduler transaction's final
    /// recheck. Like Linux `irq_work_single()`, the drain itself only consumes
    /// the claimed batch.
    pub(in crate::sched::system::task_system) fn drain_owner_work(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
    ) -> Result<(), TaskError> {
        let policy_pending = cpu.remote().owner_control_inbox().has_pending();
        let _drain = policy_pending
            .then(|| self.drain_owner_control_inner(cpu.as_mut()))
            .transpose()?;

        Ok(())
    }
}
