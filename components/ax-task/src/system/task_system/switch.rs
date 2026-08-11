//! Owner selection, schedule-out, and switch-handoff construction.

use super::*;
use crate::scheduler::RtEligibility;

pub(super) struct OwnerScheduleOut {
    pub(super) migration: Option<PreparedMigrationDelivery>,
}

impl TaskSystem {
    /// Completes every owner-side selection through the same balance and
    /// one-shot programming sequence.
    ///
    /// Forced block and exit paths select a successor just like preemption and
    /// yield. Keeping their tail common prevents a tickless CPU from retaining
    /// the outgoing thread's budget or service deadline after the switch plan
    /// has already committed a different scheduling class.
    pub(super) fn finish_owner_selection(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        decision: ScheduleDecision,
    ) -> ScheduleDecision {
        // Selection, lifecycle, and switch-handoff state are already committed
        // before this tail. Reporting a recoverable error would let block or
        // yield callers attempt to resume an outgoing thread that is no longer
        // current, so runtime failures beyond this boundary are fatal.
        self.notify_overloaded_owners_after_priority_drop(
            cpu.owner(),
            decision.previous_urgency,
            decision.next_urgency,
        );
        if self.owner_balance_work_pending(cpu.as_ref().get_ref(), decision.next())
            && self
                .service_owner_balance(cpu.as_mut(), decision.next())
                .is_err()
        {
            task_runtime::fatal_invariant(0x5343_0001, decision.next().as_u64() as usize);
        }
        if self
            .program_local_timer(
                cpu.as_mut(),
                SchedulerDeadlineDerivationSource::ScheduleSelection,
            )
            .is_err()
        {
            task_runtime::fatal_invariant(0x5343_0002, decision.next().as_u64() as usize);
        }
        decision
    }

    /// Commits one running owner either to its local queue, a migration
    /// handoff, or Deadline throttle state.
    ///
    /// `task_cpu/on_rq/on_cpu` are published as one orthogonal fact tuple;
    /// switch-transient ownership remains exclusively in `SwitchHandoff`.
    pub(super) fn schedule_out_owner_running_in_rq(
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
            || placement.execution_cpu() != Some(owner)
            || placement.on_cpu() != Some(owner)
        {
            task_runtime::fatal_invariant(0x5343_1102, core.id().as_u64() as usize);
        }

        // Linux's smp_mb__after_spinlock() orders prior userspace accesses
        // before rq->curr can publish a different task or a kernel thread.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

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
                        current_entity,
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
            core.transition_state(ThreadState::Ready)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5343_1108, core.id().as_u64() as usize)
                });
            placement.put_prev_idle(owner);
            return OwnerScheduleOut { migration: None };
        }
        let linked_policy = retained_current.then_some(current_policy);
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
            sched.policy.install_active(active);
        }

        if let Some((target, migration)) = prepared_migration {
            if retained_current {
                let active = transaction.deactivate_task(core.id()).into_active();
                sched.policy.install_active(active);
                let dispatch = transaction.take_current().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1105, core.id().as_u64() as usize)
                });
                if dispatch.thread() != core.id() || dispatch.into_active().is_some() {
                    task_runtime::fatal_invariant(0x5343_1106, core.id().as_u64() as usize);
                }
            } else {
                transaction.deactivate_unlinked_current(core.id());
            }
            sched
                .transition(&core, ThreadState::Ready)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5343_1109, core.id().as_u64() as usize)
                });
            placement.begin_migration(owner, target);
            core.set_wake_cpu_hint(target);
            return OwnerScheduleOut {
                migration: Some(migration),
            };
        }

        let current_entity = if retained_current {
            transaction
                .linked_current_entity_mut(core.id())
                .cloned()
                .unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_110a, core.id().as_u64() as usize)
                })
        } else {
            sched.policy.active().entity().clone()
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
            sched
                .transition(&core, ThreadState::Ready)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5343_110b, core.id().as_u64() as usize)
                });
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

        // Hide the outgoing dispatch while queue placement computes EEVDF
        // virtual time, but retain it until enqueue commits. A typed enqueue
        // failure can therefore restore the Running owner without publishing
        // a transient `current = None` state.
        sched
            .transition(&core, ThreadState::Ready)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5343_110c, core.id().as_u64() as usize)
            });
        let preempts_current = if retained_current {
            let queued_entity = transaction.put_prev_task(core.id(), reason);
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
            false
        } else {
            self.link_owner_ready_thread_locked(owner, transaction, &core, sched, reason)
        };
        if preempts_current {
            cpu.request_reschedule();
        }
        OwnerScheduleOut { migration: None }
    }

    pub(super) fn select_fair_active_cpu(
        &self,
        affinity: &CpuSet,
        excluded: Option<CpuId>,
    ) -> Option<CpuId> {
        self.cpu_remotes
            .iter()
            .enumerate()
            .filter_map(|(index, remote)| {
                let cpu = CpuId::new(index as u32);
                (Some(cpu) != excluded && remote.accepts_placement() && affinity.contains(cpu))
                    .then(|| (remote.queued_summary(), cpu))
            })
            .min_by_key(|(load, cpu)| (*load, cpu.as_u32()))
            .map(|(_, cpu)| cpu)
    }

    /// Linux `select_fallback_rq()` for lifecycle and affinity recovery.
    ///
    /// This path is intentionally topology-only. RT/DL urgency decisions are
    /// made by cpupri/cpudl before fallback is considered, while an invalid or
    /// offline previous CPU still needs one allowed active rq on which the
    /// task can exist.
    pub(super) fn select_fallback_active_cpu(
        &self,
        affinity: &CpuSet,
        excluded: Option<CpuId>,
    ) -> Option<CpuId> {
        self.cpu_remotes
            .iter()
            .enumerate()
            .map(|(index, remote)| (CpuId::new(index as u32), remote))
            .find_map(|(cpu, remote)| {
                (Some(cpu) != excluded && affinity.contains(cpu) && remote.accepts_placement())
                    .then_some(cpu)
            })
    }

    pub(super) fn select_priority_cpu(
        &self,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        affinity: &CpuSet,
        preferred: Option<CpuId>,
        excluded: Option<CpuId>,
    ) -> Option<CpuId> {
        let accepts = |cpu: CpuId| {
            Some(cpu) != excluded
                && self
                    .cpu_remotes
                    .get(cpu.as_usize())
                    .is_some_and(|remote| remote.accepts_placement())
        };
        // Linux find_lowest_rq()/find_later_rq() never enter cpupri/cpudl
        // when nr_cpus_allowed is one. The affinity owner is authoritative in
        // that case: priority indexes cannot discover a different target.
        if let Some(cpu) = affinity.sole_cpu() {
            return (Some(cpu) != excluded && accepts(cpu)).then_some(cpu);
        }
        let indexed = match policy {
            SchedulePolicy::KernelStop | SchedulePolicy::Fair { .. } => None,
            SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => self
                .root_domain
                .find_lowest_rt_cpu(priority, affinity, preferred, accepts),
            SchedulePolicy::Deadline(_) => entity
                .deadline()
                .and_then(DeadlineEntity::absolute_deadline_ns)
                .and_then(|absolute_deadline_ns| {
                    self.root_domain.find_later_deadline_cpu(
                        absolute_deadline_ns,
                        affinity,
                        preferred,
                        accepts,
                    )
                }),
        };
        let previous = || preferred.filter(|cpu| affinity.contains(*cpu) && accepts(*cpu));
        match policy {
            SchedulePolicy::Fair { .. } => {
                previous().or_else(|| self.select_fair_active_cpu(affinity, excluded))
            }
            SchedulePolicy::KernelStop
            | SchedulePolicy::Fifo { .. }
            | SchedulePolicy::RoundRobin { .. }
            | SchedulePolicy::Deadline(_) => indexed
                .or_else(previous)
                .or_else(|| self.select_fallback_active_cpu(affinity, excluded)),
        }
    }

    /// Linux `find_lowest_rq()` / `find_later_rq()` for an already queued
    /// RT or Deadline push candidate.
    ///
    /// Unlike wake placement, push has no general placement fallback: the
    /// candidate may leave its owner only when cpupri/cpudl identifies a CPU
    /// on which it can preempt the currently published class state. A stale
    /// index is only a hint and the migration transaction revalidates CPU
    /// admission and affinity before detaching the source entity.
    pub(super) fn select_rt_deadline_push_cpu(
        &self,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        affinity: &CpuSet,
        source: CpuId,
    ) -> Option<CpuId> {
        if affinity.sole_cpu().is_some() {
            return None;
        }
        let accepts = |cpu: CpuId| {
            cpu != source
                && self
                    .cpu_remotes
                    .get(cpu.as_usize())
                    .is_some_and(|remote| remote.accepts_placement() && remote.is_scheduler_ready())
        };
        match policy {
            SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => self
                .root_domain
                .find_lowest_rt_cpu(priority, affinity, None, accepts),
            SchedulePolicy::Deadline(_) => entity
                .deadline()
                .and_then(DeadlineEntity::absolute_deadline_ns)
                .and_then(|absolute_deadline_ns| {
                    self.root_domain.find_later_deadline_cpu(
                        absolute_deadline_ns,
                        affinity,
                        None,
                        accepts,
                    )
                }),
            SchedulePolicy::KernelStop | SchedulePolicy::Fair { .. } => None,
        }
    }

    pub(super) fn pick_owner_next_in_rq(
        &self,
        cpu: Pin<&mut CpuLocal>,
        transaction: &mut OwnerRqTxn<'_>,
        _outgoing: Option<ThreadId>,
    ) -> OwnerNext {
        let rt_eligibility = if !transaction.rt_is_effectively_throttled() {
            RtEligibility::Runnable
        } else {
            RtEligibility::Throttled
        };
        self.pick_owner_next_with_rt_eligibility(cpu, transaction, rt_eligibility)
    }

    /// Selects the sole bootstrap task before RT runtime and root-domain
    /// publication are enabled for this CPU.
    ///
    /// The bootstrap API accepts only a Fair task, so consulting online RT
    /// throttling state here would cross the Linux `sched_init()` boundary.
    pub(super) fn pick_owner_bootstrap_in_rq(
        &self,
        cpu: Pin<&mut CpuLocal>,
        transaction: &mut OwnerRqTxn<'_>,
    ) -> OwnerNext {
        self.pick_owner_next_with_rt_eligibility(cpu, transaction, RtEligibility::Runnable)
    }

    fn pick_owner_next_with_rt_eligibility(
        &self,
        cpu: Pin<&mut CpuLocal>,
        transaction: &mut OwnerRqTxn<'_>,
        rt_eligibility: RtEligibility,
    ) -> OwnerNext {
        let owner = cpu.owner();
        let Some(queued) = transaction.pick_next_task(rt_eligibility) else {
            let (core, active, metadata, rt_quota_exempt) =
                transaction.take_idle_schedule().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1110, owner.as_u32() as usize)
                });
            let placement = core.sched().placement();
            if core.state() != ThreadState::Ready
                || placement.queued_cpu() != Some(owner)
                || placement.on_cpu().is_some_and(|cpu| cpu != owner)
                || placement.requested_migration().is_some()
            {
                task_runtime::fatal_invariant(0x5343_1111, core.id().as_u64() as usize);
            }
            placement.set_next_idle(owner);
            core.transition_state(ThreadState::Running)
                .unwrap_or_else(|_| {
                    task_runtime::fatal_invariant(0x5343_1112, core.id().as_u64() as usize)
                });
            let dispatch = Self::owner_dispatch_from_rq(
                &core,
                CurrentClassState::Owned(active),
                metadata,
                rt_quota_exempt,
                transaction.clock().task(),
            );
            transaction.install_current(dispatch);
            return OwnerNext { core };
        };

        let core = Arc::clone(queued.core());
        let placement = core.sched().placement();
        let metadata = queued.metadata().clone();
        if core.state() != ThreadState::Ready
            || placement.queued_cpu() != Some(owner)
            || placement.requested_migration().is_some()
            || !metadata.affinity.contains(owner)
        {
            transaction.rollback_pick(queued);
            task_runtime::fatal_invariant(0x5343_1113, core.id().as_u64() as usize);
        }
        let rt_quota_exempt = queued.rt_quota_exempt();
        transaction.set_next_task(&queued);
        let schedule = match queued {
            PickedThread::Owned(thread) => CurrentClassState::Owned(thread.active),
            PickedThread::Linked(thread) => CurrentClassState::Linked {
                policy: thread.policy,
            },
        };
        placement.set_next_task(owner);
        core.transition_state(ThreadState::Running)
            .unwrap_or_else(|_| {
                task_runtime::fatal_invariant(0x5343_1114, core.id().as_u64() as usize)
            });
        let dispatch = Self::owner_dispatch_from_rq(
            &core,
            schedule,
            metadata,
            rt_quota_exempt,
            transaction.clock().task(),
        );
        transaction.install_current(dispatch);
        OwnerNext { core }
    }

    pub(super) fn stage_switch_handoff(
        mut cpu: Pin<&mut CpuLocal>,
        previous: Option<ThreadId>,
        previous_core: Option<Arc<ThreadCore>>,
        next: Arc<ThreadCore>,
        migration: Option<PreparedMigrationDelivery>,
    ) {
        match previous {
            Some(previous) if previous != next.id() => {
                let previous_core = previous_core.unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1115, previous.as_u64() as usize)
                });
                if previous_core.id() != previous {
                    task_runtime::fatal_invariant(0x5343_1116, previous.as_u64() as usize);
                }
                cpu.as_mut()
                    .stage_switch_handoff(previous_core, next, migration)
                    .unwrap_or_else(|_| {
                        task_runtime::fatal_invariant(0x5343_1117, previous.as_u64() as usize)
                    });
            }
            _ if migration.is_none() => {}
            _ => task_runtime::fatal_invariant(0x5343_1118, next.id().as_u64() as usize),
        }
    }

    pub(super) fn owner_switch_plan(
        previous: Option<&Arc<ThreadCore>>,
        previous_endpoint: Option<SwitchEndpoint>,
        next: &Arc<ThreadCore>,
        next_endpoint: SwitchEndpoint,
        switch_reason: SwitchReason,
        timestamp_ns: u64,
    ) -> ScheduleDecision {
        assert_eq!(previous.is_some(), previous_endpoint.is_some());
        assert_eq!(next.id(), next_endpoint.thread());
        ScheduleDecision {
            previous: previous.map(|core| core.id()),
            next: next.id(),
            previous_endpoint,
            next_endpoint,
            previous_urgency: previous.map(|core| core.effective_scheduling_urgency()),
            next_urgency: next.effective_scheduling_urgency(),
            switch_reason,
            timestamp_ns,
        }
    }
}
