//! Selection under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    pub(in crate::sched::system::task_system) fn pick_owner_next_in_rq(
        &self,
        cpu: Pin<&mut CpuLocal>,
        transaction: &mut OwnerRqTxn<'_>,
        outgoing_delayed: Option<(&ThreadCore, &mut ThreadSchedState)>,
    ) -> OwnerNext {
        let rt_eligibility = if !transaction.rt_is_effectively_throttled() {
            RtEligibility::Runnable
        } else {
            RtEligibility::Throttled
        };
        self.pick_owner_next_with_rt_eligibility(
            cpu,
            transaction,
            rt_eligibility,
            outgoing_delayed,
            None,
        )
    }

    /// Preserves Linux EEVDF's protected-current identity after ax-task has
    /// returned an outgoing runnable Fair task to its owner tree.
    pub(in crate::sched::system::task_system) fn pick_owner_next_after_preemption_in_rq(
        &self,
        cpu: Pin<&mut CpuLocal>,
        transaction: &mut OwnerRqTxn<'_>,
        previous: Option<ThreadId>,
    ) -> OwnerNext {
        let rt_eligibility = if !transaction.rt_is_effectively_throttled() {
            RtEligibility::Runnable
        } else {
            RtEligibility::Throttled
        };
        self.pick_owner_next_with_rt_eligibility(cpu, transaction, rt_eligibility, None, previous)
    }

    /// Selects the sole bootstrap task before RT runtime and root-domain
    /// publication are enabled for this CPU.
    ///
    /// The bootstrap API accepts only a Fair task, so consulting online RT
    /// throttling state here would cross the Linux `sched_init()` boundary.
    pub(in crate::sched::system::task_system) fn pick_owner_bootstrap_in_rq(
        &self,
        cpu: Pin<&mut CpuLocal>,
        transaction: &mut OwnerRqTxn<'_>,
    ) -> OwnerNext {
        self.pick_owner_next_with_rt_eligibility(
            cpu,
            transaction,
            RtEligibility::Runnable,
            None,
            None,
        )
    }

    /// Continues an RT yield directly from the class whose rq-linked current
    /// was rotated. The caller must prove the static higher-class prefix is
    /// empty and RT bandwidth still permits selection.
    #[inline(always)]
    pub(in crate::sched::system::task_system) fn pick_owner_realtime_after_yield_in_rq(
        &self,
        owner: CpuId,
        transaction: &mut OwnerRqTxn<'_>,
        queued: LinkedRqTaskRef,
    ) -> OwnerNext {
        self.install_owner_realtime_picked_in_rq(owner, transaction, queued)
    }

    /// Installs one RT selection without rebuilding the generic class result.
    #[inline(always)]
    pub(super) fn install_owner_realtime_picked_in_rq(
        &self,
        owner: CpuId,
        transaction: &mut OwnerRqTxn<'_>,
        queued: LinkedRqTaskRef,
    ) -> OwnerNext {
        transaction.set_next_realtime_task(queued);
        let (thread, policy_ref, urgency) = {
            let linked = queued.thread();
            // SAFETY: the selected RT node remains linked through switch tail.
            let policy_ref =
                unsafe { SchedulerPolicyRef::from_scheduler_owned(linked.policy_ref()) };
            (
                linked.id,
                policy_ref,
                linked.policy_ref().scheduling_urgency(),
            )
        };
        if transaction.has_pushable_class_tasks(SchedulingClass::Realtime) {
            self.root_domain
                .start_rt_deadline_push_from(RootDomainPushClass::Realtime, owner);
        }
        queued
            .thread()
            .core
            .sched()
            .placement()
            .set_next_task(owner);
        transaction.set_linked_task_current(queued, transaction.clock().task());
        let current = transaction.current_core_ref().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1113, thread.as_u64() as usize)
        });
        // SAFETY: the selected RT node remains rq-owned through switch tail.
        let core = unsafe { SchedulerThreadRef::from_scheduler_owned(current) };
        OwnerNext {
            core,
            policy: policy_ref,
            urgency,
        }
    }

    /// Installs one class-owned selection as Linux's `set_next_task()` result.
    #[inline(always)]
    pub(super) fn install_owner_picked_in_rq(
        &self,
        owner: CpuId,
        transaction: &mut OwnerRqTxn<'_>,
        queued: PickedThread,
    ) -> OwnerNext {
        transaction.set_next_task(&queued);
        let next_policy = queued.policy();
        // SAFETY: selection transfers the boxed active record into rq current
        // or retains its linked node through the context-switch tail.
        let next_policy_ref =
            unsafe { SchedulerPolicyRef::from_scheduler_owned(queued.policy_ref()) };

        // Linux set_next_task_{rt,dl} queues its class push callback after the
        // preempted task has become pushable in the same rq transaction.
        if let Some(class) = super::balance::push_class_for_policy(next_policy)
            && transaction.has_pushable_class_tasks(class.scheduling_class())
        {
            self.root_domain.start_rt_deadline_push_from(class, owner);
        }

        let thread = match queued {
            PickedThread::Owned(queued) => {
                let thread = queued.id;
                let core = queued.core;
                core.sched().placement().set_next_task(owner);
                let dispatch = CurrentDispatch::owned(
                    core,
                    queued.active,
                    queued.metadata,
                    queued.rt_quota_exempt,
                    transaction.clock().task(),
                );
                transaction.set_task_current(dispatch);
                thread
            }
            PickedThread::Linked(queued) => {
                let linked = queued.thread();
                let core = Arc::as_ref(&linked.core);
                core.sched().placement().set_next_task(owner);
                transaction.set_linked_task_current(queued, transaction.clock().task());
                linked.id
            }
        };
        let current = transaction.current_core_ref().unwrap_or_else(|| {
            task_runtime::fatal_invariant(0x5343_1113, thread.as_u64() as usize)
        });
        // SAFETY: the selected current is now owned by CurrentDispatch for
        // Fair/stop or by its still-linked RT/DL node. Both ownership sources
        // remain live through the incoming switch tail.
        let core = unsafe { SchedulerThreadRef::from_scheduler_owned(current) };

        let urgency = if matches!(next_policy, SchedulePolicy::Deadline(_)) {
            transaction.current_scheduling_urgency().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1113, thread.as_u64() as usize)
            })
        } else {
            next_policy.scheduling_urgency()
        };
        OwnerNext {
            core,
            policy: next_policy_ref,
            urgency,
        }
    }

    pub(super) fn pick_owner_next_with_rt_eligibility(
        &self,
        cpu: Pin<&mut CpuLocal>,
        transaction: &mut OwnerRqTxn<'_>,
        rt_eligibility: RtEligibility,
        mut outgoing_delayed: Option<(&ThreadCore, &mut ThreadSchedState)>,
        protected_fair_current: Option<ThreadId>,
    ) -> OwnerNext {
        let owner = cpu.owner();
        let mut skip_delayed = false;
        let mut delayed_retry_required = false;
        let queued = loop {
            let picked =
                transaction.pick_next_task(rt_eligibility, skip_delayed, protected_fair_current);

            match picked {
                Some(PickTaskResult::Continue(queued)) => break Some(queued),
                Some(PickTaskResult::Break(core)) => {
                    if let Some((outgoing, sched)) = outgoing_delayed.as_mut()
                        && core::ptr::eq(*outgoing, core.as_ref())
                    {
                        let placement = core.sched().placement();
                        if sched.lifecycle.state() != ThreadState::Blocked
                            || placement.queued_cpu() != Some(owner)
                            || !transaction.is_delayed_fair(core.id())
                        {
                            task_runtime::fatal_invariant(0x5343_1119, core.id().as_u64() as usize);
                        }
                        let thread = transaction.finish_delayed_fair_dequeue(
                            core.id(),
                            self.config.timing_granularity_ns(),
                        );
                        core.sched().install_active(sched, thread.into_active());
                        placement.finish_delayed_dequeue(owner);
                        core.set_wake_cpu_hint(owner);
                        skip_delayed = false;
                        continue;
                    }
                    // Normal ordering is p->pi_lock then rq. Linux can finish
                    // delayed dequeue directly because sched_entity lives in
                    // task_struct; ax-task must return its owned active state
                    // to task control. The inverse try-lock never waits: a
                    // concurrent waker holding the task lock wins. Keep a
                    // preemption generation pending if no other entity can be
                    // selected, so task-lock contention cannot strand a
                    // non-empty rq on the dedicated idle task.
                    let Some(mut sched) = (unsafe { core.sched().try_lock_from_owner_rq() }) else {
                        skip_delayed = true;
                        delayed_retry_required = true;
                        continue;
                    };
                    let placement = core.sched().placement();
                    if sched.lifecycle.state() != ThreadState::Blocked
                        || placement.queued_cpu() != Some(owner)
                        || !transaction.is_delayed_fair(core.id())
                    {
                        task_runtime::fatal_invariant(0x5343_1119, core.id().as_u64() as usize);
                    }
                    let thread = transaction.finish_delayed_fair_dequeue(
                        core.id(),
                        self.config.timing_granularity_ns(),
                    );
                    core.sched()
                        .install_active(&mut sched, thread.into_active());
                    placement.finish_delayed_dequeue(owner);
                    core.set_wake_cpu_hint(owner);
                    skip_delayed = false;
                }
                None => {
                    if delayed_retry_required {
                        cpu.request_reschedule(RescheduleKind::Immediate);
                    }
                    break None;
                }
            }
        };
        let Some(queued) = queued else {
            let (core, active, metadata, rt_quota_exempt) =
                transaction.take_idle_schedule().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x5343_1110, owner.as_u32() as usize)
                });
            let policy = active.policy();
            // SAFETY: `set_idle_current` transfers this stable boxed record to
            // rq current, which retains it through the incoming switch tail.
            let policy_ref =
                unsafe { SchedulerPolicyRef::from_scheduler_owned(active.policy_ref()) };
            let urgency = active.entity().scheduling_urgency(policy);
            let placement = core.sched().placement();
            if core.state() != ThreadState::Running
                || placement.queued_cpu() != Some(owner)
                || placement.on_cpu().is_some_and(|cpu| cpu != owner)
                || placement.requested_migration().is_some()
            {
                task_runtime::fatal_invariant(0x5343_1111, core.id().as_u64() as usize);
            }
            placement.set_next_idle(owner);
            let thread = core.id();
            let dispatch = CurrentDispatch::owned(
                core,
                active,
                metadata,
                rt_quota_exempt,
                transaction.clock().task(),
            );
            transaction.set_idle_current(dispatch);
            let current = transaction.current_core_ref().unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x5343_1111, thread.as_u64() as usize)
            });
            // SAFETY: `set_idle_current` installed an owned current dispatch;
            // that dispatch retains the Arc through the incoming switch tail.
            let core = unsafe { SchedulerThreadRef::from_scheduler_owned(current) };
            return OwnerNext {
                core,
                policy: policy_ref,
                urgency,
            };
        };

        self.install_owner_picked_in_rq(owner, transaction, queued)
    }
}
