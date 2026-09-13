//! Schedule out under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Returns whether ordinary preemption can complete under the rq lock.
    ///
    /// Linux's `__schedule()` handles the common put-prev path with only
    /// `rq->lock`. Ax-task needs the task scheduler lock only when a migration
    /// request or Deadline timer ownership must cross the task/rq boundary.
    #[inline(always)]
    pub(in crate::sched::system::task_system) fn prepare_owner_rq_schedule_out(
        &self,
        transaction: &OwnerRqTxn<'_>,
        core: &ThreadCore,
    ) -> Option<OwnerRqScheduleOut> {
        let dispatch = transaction.current().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1116, core.id().as_u64() as usize)
        });
        debug_assert_eq!(dispatch.thread(), core.id());
        let placement = core.sched().placement();
        debug_assert_eq!(core.state(), ThreadState::Running);
        debug_assert_eq!(placement.queued_cpu(), Some(transaction.owner()));
        debug_assert_eq!(placement.on_cpu(), Some(transaction.owner()));
        if placement.requested_migration().is_some()
            || dispatch.metadata().deadline_bandwidth_scaled != 0
            || matches!(dispatch.schedule_policy(), SchedulePolicy::Deadline(_))
        {
            return None;
        }
        let thread = core.id();
        if dispatch.is_dedicated_idle() {
            Some(OwnerRqScheduleOut::Idle { thread })
        } else if let Some(previous) = dispatch.linked_task_ref() {
            Some(OwnerRqScheduleOut::LinkedRealtime { previous })
        } else {
            Some(OwnerRqScheduleOut::Unlinked { thread })
        }
    }

    /// Performs the common Linux `put_prev_task()` path with rq as sole owner.
    #[inline(always)]
    pub(in crate::sched::system::task_system) fn schedule_out_owner_rq_owned(
        &self,
        transaction: &mut OwnerRqTxn<'_>,
        ownership: OwnerRqScheduleOut,
        reason: EnqueueReason,
    ) -> OwnerRqScheduledOut {
        let thread = match &ownership {
            OwnerRqScheduleOut::Idle { thread } | OwnerRqScheduleOut::Unlinked { thread } => {
                *thread
            }
            OwnerRqScheduleOut::LinkedRealtime { previous } => previous.thread().id,
        };
        if !matches!(reason, EnqueueReason::Preempted | EnqueueReason::Yield) {
            task_runtime::fatal_invariant(0x5343_111b, thread.as_u64() as usize);
        }
        // Pairs prior task accesses with publication of a different rq->curr.
        crate::runtime::lock::smp_mb_after_spinlock();
        match ownership {
            OwnerRqScheduleOut::Idle { .. } => {
                let current = transaction.current().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1105, thread.as_u64() as usize)
                });
                debug_assert_eq!(current.thread(), thread);
                let endpoint = current.switch_endpoint();
                let policy = current.schedule_policy_ref();
                let urgency = policy.scheduling_urgency();
                let fifo = matches!(policy, SchedulePolicy::Fifo { .. });
                let dispatch = transaction.take_current().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1105, thread.as_u64() as usize)
                });
                let (core, active) = dispatch.into_runtime_core_and_active();
                let active = active.unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1107, thread.as_u64() as usize)
                });
                transaction.return_idle_schedule(thread, active);
                OwnerRqScheduledOut {
                    core: PreviousSwitchOwnership::retained(core),
                    endpoint,
                    fifo,
                    urgency,
                    realtime_yield_head: None,
                }
            }
            OwnerRqScheduleOut::LinkedRealtime { previous } => {
                let previous_thread = previous.thread();
                debug_assert_eq!(previous_thread.id, thread);
                let endpoint = previous_thread.switch_endpoint();
                let policy = previous_thread.policy_ref();
                let urgency = policy.scheduling_urgency();
                let fifo = matches!(policy, SchedulePolicy::Fifo { .. });
                let migration_capable = previous_thread.migration_capable;
                // SAFETY: the linked RT node pins the previous core until the
                // owner-rq lock baton reaches context-switch completion.
                let core = unsafe {
                    SchedulerThreadRef::from_scheduler_owned(previous_thread.core.as_ref())
                };
                let realtime_yield_head = if reason == EnqueueReason::Yield {
                    Some(transaction.yield_realtime_current(thread))
                } else {
                    None
                };
                transaction.put_prev_realtime_task(thread, migration_capable);
                OwnerRqScheduledOut {
                    core: PreviousSwitchOwnership::scheduler_owned(core),
                    endpoint,
                    fifo,
                    urgency,
                    realtime_yield_head,
                }
            }
            OwnerRqScheduleOut::Unlinked { .. } => {
                let current = transaction.current().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1105, thread.as_u64() as usize)
                });
                debug_assert_eq!(current.thread(), thread);
                let endpoint = current.switch_endpoint();
                let policy = current.schedule_policy();
                let urgency = policy.scheduling_urgency();
                let fifo = matches!(policy, SchedulePolicy::Fifo { .. });
                let core = transaction.current_core().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1105, thread.as_u64() as usize)
                });
                let queued_entity = transaction.put_prev_unlinked_current(thread, reason);
                core.publish_effective_schedule(policy, &queued_entity);
                OwnerRqScheduledOut {
                    core: PreviousSwitchOwnership::retained(core),
                    endpoint,
                    fifo,
                    urgency,
                    realtime_yield_head: None,
                }
            }
        }
    }

    /// Commits one running owner either to its local queue, a migration
    /// handoff, or Deadline throttle state.
    ///
    /// `task_cpu/on_rq/on_cpu` are published as one orthogonal fact tuple;
    /// switch-transient ownership remains exclusively in `SwitchHandoff`.
    pub(in crate::sched::system::task_system) fn schedule_out_owner_running_in_rq(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        transaction: &mut OwnerRqTxn<'_>,
        core: Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> OwnerScheduleOut {
        self.ensure_owner_cpu_online(&cpu).unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x5343_1101, cpu.owner().as_u32() as usize)
        });
        let owner = cpu.owner();
        let placement = core.sched().placement();
        let retained_current = transaction.is_linked_current(core.id());
        if sched.lifecycle.state() != ThreadState::Running
            || placement.queued_cpu() != Some(owner)
            || placement.on_cpu() != Some(owner)
        {
            task_runtime::fatal_invariant(0x5343_1102, core.id().as_u64() as usize);
        }

        // Linux's smp_mb__after_spinlock() orders prior userspace accesses
        // before rq->curr can publish a different task or a kernel thread.
        crate::runtime::lock::smp_mb_after_spinlock();

        let migration_requested =
            placement.requested_migration().is_some() || !sched.affinity.affinity.contains(owner);
        let current_policy = transaction
            .current()
            .map(CurrentDispatch::schedule_policy)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1111, core.id().as_u64() as usize)
            });
        let current_entity = transaction.current_scheduling_entity().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1112, core.id().as_u64() as usize)
        });
        let prepared_migration = migration_requested.then(|| {
            let target = placement
                .requested_migration()
                .filter(|target| {
                    *target != owner
                        && sched.affinity.affinity.contains(*target)
                        && self
                            .cpu_remotes
                            .get(target.as_usize())
                            .is_some_and(|remote| remote.accepts_placement())
                })
                .or_else(|| {
                    self.select_priority_cpu(
                        current_policy,
                        Some(current_entity),
                        &sched.affinity.affinity,
                        None,
                        Some(owner),
                    )
                })
                .unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1103, core.id().as_u64() as usize)
                });
            let migration = self
                .prepare_owner_migration(&core, owner, target)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5343_1104, core.id().as_u64() as usize)
                });
            (target, migration)
        });
        if prepared_migration.is_some() {
            transaction
                .capture_current_fair_migration(core.id(), self.config.timing_granularity_ns());
        }
        if transaction.idle() == Some(core.id()) {
            let dispatch = transaction.take_current().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1105, core.id().as_u64() as usize)
            });
            if dispatch.thread() != core.id() {
                task_runtime::fatal_invariant(0x5343_1106, core.id().as_u64() as usize);
            }
            let active = dispatch.into_active().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1107, core.id().as_u64() as usize)
            });
            transaction.return_idle_schedule(core.id(), active);
            placement.put_prev_idle(owner);
            return OwnerScheduleOut { migration: None };
        }
        let linked_policy = retained_current.then_some(current_policy);
        if let Some((target, migration)) = prepared_migration {
            if retained_current {
                let active = transaction.deactivate_task(core.id()).into_active();
                core.sched().install_active(sched, active);
                let dispatch = transaction.take_current().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1105, core.id().as_u64() as usize)
                });
                if dispatch.thread() != core.id() || dispatch.into_active().is_some() {
                    task_runtime::fatal_invariant(0x5343_1106, core.id().as_u64() as usize);
                }
            } else {
                transaction.deactivate_unlinked_current(core.id());
                let dispatch = transaction.take_current().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1105, core.id().as_u64() as usize)
                });
                if dispatch.thread() != core.id() {
                    task_runtime::fatal_invariant(0x5343_1106, core.id().as_u64() as usize);
                }
                let active = dispatch.into_active().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_110a, core.id().as_u64() as usize)
                });
                core.sched().install_active(sched, active);
            }
            placement.begin_migration(owner, target);
            core.set_wake_cpu_hint(target);
            return OwnerScheduleOut {
                migration: Some(migration),
            };
        }

        if !retained_current {
            let dispatch = transaction.take_current().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1105, core.id().as_u64() as usize)
            });
            if dispatch.thread() != core.id() {
                task_runtime::fatal_invariant(0x5343_1106, core.id().as_u64() as usize);
            }
            let active = dispatch.into_active().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_110a, core.id().as_u64() as usize)
            });
            core.sched().install_active(sched, active);
        }

        let current_entity = if retained_current {
            transaction
                .linked_current_entity_mut(core.id())
                .cloned()
                .unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_110a, core.id().as_u64() as usize)
                })
        } else {
            core.sched().active(sched).entity().clone()
        };
        if current_entity.is_deadline_throttled() {
            if !retained_current {
                task_runtime::fatal_invariant(0x5343_110b, core.id().as_u64() as usize);
            }
            transaction
                .throttle_current_deadline(core.id())
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5343_110b, core.id().as_u64() as usize)
                });
            let dispatch = transaction.take_current().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1105, core.id().as_u64() as usize)
            });
            if dispatch.thread() != core.id() || dispatch.into_active().is_some() {
                task_runtime::fatal_invariant(0x5343_1106, core.id().as_u64() as usize);
            }
            // A throttled DL task remains TASK_ON_RQ_QUEUED while its class
            // entity is absent from the EDF tree and rq->nr_running.
            placement.put_prev(owner);
            if self
                .refresh_owner_deadline_timers_in_rq(
                    &core,
                    sched,
                    cpu.as_mut(),
                    now_ns,
                    transaction,
                )
                .is_some()
            {
                cpu.request_scheduler_work();
            }
            return OwnerScheduleOut { migration: None };
        }

        if retained_current {
            // Timer replacement is the only recoverable preparation in the
            // retained RT/DL path. Complete it before mutating runqueue or
            // placement ownership, like Linux prepares class state before
            // the rq-locked put-prev/set-next commit.
            if self
                .refresh_owner_deadline_timers_in_rq(
                    &core,
                    sched,
                    cpu.as_mut(),
                    now_ns,
                    transaction,
                )
                .is_some()
            {
                cpu.request_scheduler_work();
            }
        }

        // Keep Linux `TASK_RUNNING` while queue placement computes EEVDF
        // virtual time. The retained dispatch remains available until enqueue
        // commits, so no second lifecycle fact is needed for put-prev.
        let enqueue = if retained_current {
            if reason == EnqueueReason::Yield
                && matches!(
                    current_policy,
                    SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. }
                )
            {
                transaction.yield_realtime_current(core.id());
            }
            let queued_entity = transaction.put_prev_task(core.id());
            let dispatch = transaction.take_current().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1105, core.id().as_u64() as usize)
            });
            if dispatch.thread() != core.id() || dispatch.into_active().is_some() {
                task_runtime::fatal_invariant(0x5343_1106, core.id().as_u64() as usize);
            }
            placement.put_prev(owner);
            core.publish_effective_schedule(
                linked_policy.expect("retained current must publish linked policy"),
                &queued_entity,
            );
            core.set_wake_cpu_hint(owner);
            dispatch::OwnerReadyEnqueue {
                reschedule: None,
                scheduler_deadline_refresh_required: false,
            }
        } else {
            self.link_owner_ready_thread_locked(owner, transaction, &core, sched, reason)
        };
        if let Some(kind) = enqueue.reschedule {
            cpu.request_reschedule(kind);
        }
        // This transaction is already inside the owner scheduling decision;
        // its final deadline derivation consumes any enqueue refresh edge.
        let _scheduler_deadline_refresh_consumed_by_owner =
            enqueue.scheduler_deadline_refresh_required;
        OwnerScheduleOut { migration: None }
    }
}
