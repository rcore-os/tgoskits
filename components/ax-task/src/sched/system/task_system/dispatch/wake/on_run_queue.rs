//! On run queue under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    /// Completes Linux's `ttwu_runnable()` transaction without waiting for `on_cpu`.
    ///
    /// If the task still owns `TASK_ON_RQ_QUEUED`, a delayed Fair task uses
    /// `ENQUEUE_DELAYED` to cancel its pending dequeue while an ordinary task
    /// stays linked. A concurrent dequeue instead falls through to the
    /// already-reserved off-rq activation, matching `ttwu_runnable()` returning
    /// false to `try_to_wake_up()`.
    pub(super) fn wake_on_rq_locked(
        &self,
        core: &Arc<ThreadCore>,
        mut sched_guard: crate::runtime::lock::IrqTicketGuard<'_, ThreadSchedState>,
        target: CpuId,
        intent: WakeIntent,
        context: WakeTransactionContext,
    ) -> WakeResult {
        let remote = &self.cpu_remotes[target.as_usize()];
        remote.cancel_idle_pull_if_uncommitted();
        let on_rq_publication = {
            let (sched, irq_owner) = sched_guard.split_irq_owner();
            if sched.lifecycle.state() != ThreadState::Blocked {
                task_runtime::fatal_invariant(0x574b_0013, core.id().as_u64() as usize);
            }
            let mut run_queue = OwnerRqTxn::begin_nested(self, remote, &irq_owner);
            let scheduling_state = run_queue.scheduling_state(core.id());
            let revalidation = on_rq_revalidation(scheduling_state.is_some());

            match revalidation {
                OnRqRevalidation::ActivateOffRq => {
                    if sched.placement.queued_cpu().is_some()
                        || sched.placement.committed_migration_target().is_some()
                    {
                        task_runtime::fatal_invariant(0x574b_0016, core.id().as_u64() as usize);
                    }
                    run_queue.commit();
                    None
                }
                OnRqRevalidation::CommitOnRq => {
                    let (policy, fair_wake) = wake_policy_for_revalidation(
                        scheduling_state.as_ref().map(|(policy, _entity)| policy),
                        revalidation,
                    )
                    .unwrap_or_else(|| {
                        task_runtime::fatal_invariant(0x574b_0016, core.id().as_u64() as usize)
                    });
                    if sched.placement.queued_cpu() != Some(target) {
                        task_runtime::fatal_invariant(0x574b_0016, core.id().as_u64() as usize);
                    }
                    let action = if fair_wake {
                        on_rq_wake_action(run_queue.is_delayed_fair(core.id()))
                    } else {
                        OnRqWakeAction::PublishAlreadyQueued
                    };
                    let on_cpu = match sched.placement.on_cpu() {
                        None => false,
                        Some(owner) if owner == target => true,
                        Some(owner) => {
                            task_runtime::fatal_invariant(0x574b_0018, owner.as_u32() as usize)
                        }
                    };
                    if fair_wake
                        && run_queue.current().is_some_and(|current| {
                            matches!(current.schedule_policy(), SchedulePolicy::Fair { .. })
                        })
                    {
                        let _ = run_queue.settle_current(0);
                    }
                    let current_fair = fair_wake
                        .then(|| run_queue.current_fair_contender())
                        .flatten();

                    if fair_wake {
                        run_queue.update_fair_virtual_time(current_fair);
                    }
                    let (wakeup_entity, owner_work_required) = match action {
                        OnRqWakeAction::ReactivateDelayedFair => {
                            let enqueue = run_queue.reactivate_delayed_fair(
                                core.id(),
                                current_fair,
                                self.config.timing_granularity_ns(),
                            );
                            (
                                enqueue.entity().clone(),
                                enqueue.scheduler_deadline_refresh_required(),
                            )
                        }
                        OnRqWakeAction::PublishAlreadyQueued => (
                            scheduling_state
                                .map(|(_policy, entity)| entity)
                                .unwrap_or_else(|| {
                                    task_runtime::fatal_invariant(
                                        0x574b_0014,
                                        core.id().as_u64() as usize,
                                    )
                                }),
                            false,
                        ),
                    };

                    if fair_wake && matches!(action, OnRqWakeAction::ReactivateDelayedFair) {
                        run_queue.update_fair_virtual_time(current_fair);
                    }
                    let fair_virtual_time = if fair_wake {
                        run_queue.virtual_time()
                    } else {
                        Default::default()
                    };

                    let reschedule_pending = remote.immediate_preemption_requested();
                    let preemption = if on_rq_wake_preemption_required(on_cpu) {
                        run_queue.wakeup_preempt_with_intent(
                            core.id(),
                            policy,
                            &wakeup_entity,
                            fair_virtual_time,
                            WakePreemptionContext::new(
                                intent,
                                EqualRtWakeAction::PreserveFifoOrder,
                                reschedule_pending,
                            ),
                        )
                    } else {
                        WakePreemptionDecision::KeepCurrent
                    };

                    let reschedule = preemption.reschedule_kind(policy);

                    core.publish_effective_schedule(policy, &wakeup_entity);
                    core.set_wake_cpu_hint(target);
                    if sched.transition(core, ThreadState::Running).is_err() {
                        task_runtime::fatal_invariant(0x574b_0015, core.id().as_u64() as usize);
                    }
                    remote.publish_rq_scheduler_reasons(
                        reschedule,
                        owner_work_required,
                        context.producer,
                        &irq_owner,
                    );
                    run_queue.commit();
                    Some(())
                }
            }
        };

        let Some(()) = on_rq_publication else {
            if sched_guard.transition(core, ThreadState::Waking).is_err() {
                task_runtime::fatal_invariant(0x574b_0016, core.id().as_u64() as usize);
            }
            return self.activate_waking_thread_locked(core, sched_guard, target, intent, context);
        };
        drop(sched_guard);
        WakeResult::Notified
    }
}
