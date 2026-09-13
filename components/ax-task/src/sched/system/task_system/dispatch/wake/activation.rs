//! Activation under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Prepares the policy-owned state before the common wake enqueue. Keeping
    /// Fair/Deadline accounting out of the RT enqueue function leaves the
    /// Linux FIFO/RR path with one policy classification and one ownership
    /// transfer instead of repeatedly checking class-specific metadata.
    pub(super) fn prepare_wake_activation(
        &self,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        run_queue: &mut OwnerRqTxn<'_>,
        target: CpuId,
    ) -> WakeActivationPreparation {
        let mut active = core.sched().active(sched);
        let policy = active.policy();
        if matches!(policy, SchedulePolicy::Fair { .. })
            && run_queue.current().is_some_and(|current| {
                matches!(current.schedule_policy(), SchedulePolicy::Fair { .. })
            })
        {
            let _ = run_queue.settle_current(0);
        }
        let deadline_wake = matches!(policy, SchedulePolicy::Deadline(_)) && !sched.is_pi_boosted();
        if deadline_wake {
            active
                .entity_mut()
                .activate_deadline(run_queue.clock().wall().as_nanos());
        }
        let deadline_throttled = deadline_wake
            && active
                .entity()
                .deadline()
                .is_some_and(DeadlineEntity::is_throttled);
        let maintains_fair_virtual_time = active.entity().fair().is_some();
        let delayed_migration_wake = active
            .entity()
            .fair()
            .is_some_and(|fair| fair.is_delayed_migrating());
        drop(active);
        if deadline_throttled {
            self.link_owner_throttled_deadline_locked(run_queue, core, sched, target);
            return WakeActivationPreparation::Throttled;
        }

        Self::activate_deadline_bandwidth_locked(core, sched, run_queue, target);
        let current_fair = if maintains_fair_virtual_time {
            let current_fair = run_queue.current_fair_contender();
            run_queue.update_fair_virtual_time(current_fair);
            current_fair
        } else {
            None
        };
        let metadata = sched.rq_task_metadata().unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x574b_0103, core.id().as_u64() as usize)
        });
        let active = core.sched().take_active(sched);
        debug_assert_eq!(active.policy(), policy);
        WakeActivationPreparation::Ready {
            policy,
            active,
            metadata,
            current_fair,
            maintains_fair_virtual_time,
            delayed_migration_wake,
            deadline_wake,
        }
    }

    pub(super) fn activate_waking_thread_locked(
        &self,
        core: &Arc<ThreadCore>,
        mut sched_guard: crate::runtime::lock::IrqTicketGuard<'_, ThreadSchedState>,
        target: CpuId,
        intent: WakeIntent,
        context: WakeTransactionContext,
    ) -> WakeResult {
        // PREEMPT_RT disables TTWU_QUEUE. The waker therefore retains the task
        // lock, pairs this acquire wait with `finish_task()`'s release-clear of
        // `on_cpu`, then activates the task under its selected rq lock.
        // Only a Blocked-to-Waking transition reaches this wait. Both park
        // paths install a local handoff whose tail clears on_cpu without the
        // task lock. A migration handoff retains Running until its tail has
        // cleared on_cpu, so its lock-taking tail cannot be this wait's peer.

        let (sched, irq_owner) = sched_guard.split_irq_owner();
        sched.placement.wait_until_not_on_cpu();
        if sched.lifecycle.state() != ThreadState::Waking || sched.placement.on_cpu().is_some() {
            task_runtime::fatal_invariant(0x574b_0005, core.id().as_u64() as usize);
        }
        let remote = &self.cpu_remotes[target.as_usize()];
        remote.cancel_idle_pull_if_uncommitted();
        if let Some(source) = sched
            .deadline
            .bandwidth
            .reservation_owner()
            .filter(|source| *source != target)
        {
            let source_remote = &self.cpu_remotes[source.as_usize()];
            let mut source_run_queue = OwnerRqTxn::begin_nested(self, source_remote, &irq_owner);
            Self::detach_owner_deadline_bandwidth_in_rq(
                core,
                sched,
                source_remote,
                &mut source_run_queue,
            );
            source_run_queue.commit();
            // The old physical clockevent may still point at the cancelled
            // inactive/CBS timer. Its owner recomputes the base before idle;
            // a racing stale edge is harmless and will be stopped by the
            // clockevent firing transaction.
            let _delivered = Self::publish_detached_deadline_owner_work(source_remote);
        }
        let mut run_queue = OwnerRqTxn::begin_nested(self, remote, &irq_owner);

        let preparation = self.prepare_wake_activation(core, sched, &mut run_queue, target);
        let (
            policy,
            active,
            metadata,
            current_fair,
            maintains_fair_virtual_time,
            delayed_migration_wake,
            deadline_wake,
        ) = match preparation {
            WakeActivationPreparation::Throttled => {
                if sched.transition(core, ThreadState::Running).is_err() {
                    task_runtime::fatal_invariant(0x574b_0006, core.id().as_u64() as usize);
                }
                #[cfg(feature = "qperf-metrics")]
                crate::diagnostics::counters::record_direct_wake_activation();
                run_queue.commit();
                drop(sched_guard);
                self.publish_owner_deadline_refresh(core, target);
                return WakeResult::Notified;
            }
            WakeActivationPreparation::Ready {
                policy,
                active,
                metadata,
                current_fair,
                maintains_fair_virtual_time,
                delayed_migration_wake,
                deadline_wake,
            } => (
                policy,
                active,
                metadata,
                current_fair,
                maintains_fair_virtual_time,
                delayed_migration_wake,
                deadline_wake,
            ),
        };
        let queued = QueuedThread::new(
            core.id(),
            active,
            Arc::clone(core),
            sched.is_pi_boosted_rt_owner_for(policy),
            sched.affinity.affinity.is_migration_capable(),
            metadata,
        );
        let enqueue = if delayed_migration_wake {
            run_queue.enqueue_reactivated_delayed_fair_transfer(
                queued,
                current_fair,
                self.config.timing_granularity_ns(),
            )
        } else {
            run_queue.enqueue_task(queued, EnqueueReason::Wake, current_fair)
        };
        sched.placement.activate(target);

        if maintains_fair_virtual_time {
            run_queue.update_fair_virtual_time(current_fair);
        }
        let fair_virtual_time = enqueue
            .entity()
            .fair()
            .map_or(0, |_| run_queue.virtual_time());

        let reschedule_pending = remote.immediate_preemption_requested();
        let equal_rt_action =
            run_queue
                .current()
                .map_or(EqualRtWakeAction::PreserveFifoOrder, |current| {
                    self.equal_rt_wake_action(EqualRtWakeContext {
                        target,
                        current,
                        wakee_policy: policy,
                        wakee_affinity: &sched.affinity.affinity,
                        reschedule_pending,
                    })
                });
        let preemption = run_queue.wakeup_preempt_with_intent(
            core.id(),
            policy,
            enqueue.entity(),
            fair_virtual_time,
            WakePreemptionContext::new(intent, equal_rt_action, reschedule_pending),
        );

        let reschedule = preemption.reschedule_kind(policy);

        #[cfg(feature = "qperf-metrics")]
        let preempts_current = reschedule.is_some();
        core.publish_effective_schedule(policy, enqueue.entity());
        core.set_wake_cpu_hint(target);
        if sched.transition(core, ThreadState::Running).is_err() {
            task_runtime::fatal_invariant(0x574b_0006, core.id().as_u64() as usize);
        }
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::record_direct_wake_activation();
        let push_class = super::super::balance::push_class_for_policy(policy)
            .filter(|class| run_queue.has_pushable_class_tasks(class.scheduling_class()));
        let refresh_runtime = !deadline_wake && enqueue.scheduler_deadline_refresh_required();
        let local_runtime = (refresh_runtime && target == context.producer).then(|| {
            if reschedule == Some(RescheduleKind::Immediate)
                || remote.immediate_preemption_requested()
            {
                SchedulerRuntimeDeadline::Disarmed
            } else {
                run_queue.current_runtime_deadline()
            }
        });
        remote.publish_rq_scheduler_reasons(
            reschedule,
            refresh_runtime && local_runtime.is_none(),
            context.producer,
            &irq_owner,
        );
        run_queue.commit();
        if let Some(deadline) = local_runtime {
            // The outer task lock retains local IRQ exclusion after rq commit.
            // Remote updates retain their owner-work bit and are serviced later.
            task_runtime::publish_scheduler_runtime_deadline(deadline);
        }
        drop(sched_guard);
        let rt_period_started = self.activate_owner_rt_period_for_policy(target, policy);
        if rt_period_started {
            remote.request_scheduler_work();
        }

        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::record_direct_wake_enqueue();
        #[cfg(feature = "qperf-metrics")]
        if preempts_current {
            crate::diagnostics::counters::record_direct_wake_preemption();
        }
        #[cfg(feature = "qperf-metrics")]
        match preemption {
            WakePreemptionDecision::KeepCurrent => {
                crate::diagnostics::counters::record_direct_wake_current_kept()
            }
            WakePreemptionDecision::DedicatedIdlePreempted => {}
            WakePreemptionDecision::QueuedCandidateSelected => {
                crate::diagnostics::counters::record_direct_wake_queued_candidate_selected()
            }
            WakePreemptionDecision::WakeeSelected => {}
        }
        if deadline_wake {
            self.publish_owner_deadline_refresh(core, target);
        }
        if let Some(class) = push_class {
            self.root_domain.start_rt_deadline_push_from(class, target);
        }
        WakeResult::Notified
    }
}
