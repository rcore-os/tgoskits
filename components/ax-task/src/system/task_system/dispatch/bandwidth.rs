//! Deadline bandwidth ownership and rq attachment transactions.

use super::*;
use crate::WakePreemptionContext;

impl TaskSystem {
    pub(in crate::system::task_system) fn activate_owner_rt_period_for_policy(
        &self,
        owner: CpuId,
        policy: SchedulePolicy,
    ) -> bool {
        if policy.rt_priority().is_none() || !self.rt_bandwidth_enabled() {
            return false;
        }
        self.root_domain
            .activate_rt_period(owner, task_runtime::monotonic_now)
    }

    pub(in crate::system::task_system) fn link_owner_throttled_deadline_locked(
        &self,
        run_queue: &mut OwnerRqTxn<'_>,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        owner: CpuId,
    ) {
        let active = core.sched().active(sched);
        let policy = active.policy();
        let entity = active.entity().clone();
        drop(active);
        if !matches!(policy, SchedulePolicy::Deadline(_)) || !entity.is_deadline_throttled() {
            task_runtime::fatal_invariant(0x574b_1110, core.id().as_u64() as usize);
        }
        Self::activate_deadline_bandwidth_locked(core, sched, run_queue, owner);
        let metadata = sched.rq_task_metadata().unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x574b_1111, core.id().as_u64() as usize)
        });
        let active = core.sched().take_active(sched);
        run_queue.enqueue_throttled_deadline(QueuedThread::new(
            core.id(),
            active,
            Arc::clone(core),
            false,
            sched.affinity.affinity.is_migration_capable(),
            metadata,
        ));
        sched.placement.activate(owner);
        core.publish_effective_schedule(policy, &entity);
        core.set_wake_cpu_hint(owner);
    }

    pub(in crate::system::task_system) fn link_owner_ready_thread_locked(
        &self,
        owner: CpuId,
        run_queue: &mut OwnerRqTxn<'_>,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        reason: EnqueueReason,
    ) -> OwnerReadyEnqueue {
        self.link_owner_ready_thread_locked_with_context(
            owner,
            run_queue,
            core,
            sched,
            reason,
            WakePreemptionContext::new(
                WakeIntent::Normal,
                EqualRtWakeAction::PreserveFifoOrder,
                self.cpu_remotes[owner.as_usize()].immediate_preemption_requested(),
            ),
        )
    }

    pub(in crate::system::task_system) fn link_owner_ready_thread_locked_with_context(
        &self,
        owner: CpuId,
        run_queue: &mut OwnerRqTxn<'_>,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        reason: EnqueueReason,
        wake_context: WakePreemptionContext,
    ) -> OwnerReadyEnqueue {
        let active = core.sched().active(sched);
        let policy = active.policy();
        let maintains_fair_virtual_time = active.entity().fair().is_some();
        drop(active);
        let current_fair = if maintains_fair_virtual_time {
            let current_fair = run_queue.current_fair_contender();
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::record_wake_fair_vtime_update(core.id());
            run_queue.update_fair_virtual_time(current_fair);
            current_fair
        } else {
            None
        };
        let metadata = sched.rq_task_metadata().unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x574b_1102, core.id().as_u64() as usize)
        });
        let active = core.sched().take_active(sched);
        let enqueue = run_queue.enqueue_task(
            QueuedThread::new(
                core.id(),
                active,
                Arc::clone(core),
                sched.is_pi_boosted_rt_owner_for(policy),
                sched.affinity.affinity.is_migration_capable(),
                metadata,
            ),
            reason,
            current_fair,
        );
        Self::activate_deadline_bandwidth_locked(core, sched, run_queue, owner);
        if maintains_fair_virtual_time {
            #[cfg(feature = "task-test-hooks")]
            crate::task_test_hooks::record_wake_fair_vtime_update(core.id());
            run_queue.update_fair_virtual_time(current_fair);
        }
        let reschedule = if reason.checks_preemption_after_enqueue() {
            let fair_virtual_time = enqueue
                .entity()
                .fair()
                .map_or(0, |fair| run_queue.virtual_time_for_mode(fair.mode()));
            run_queue
                .wakeup_preempt_with_intent(
                    core.id(),
                    policy,
                    enqueue.entity(),
                    fair_virtual_time,
                    wake_context,
                )
                .reschedule_kind(policy)
        } else {
            None
        };
        core.publish_effective_schedule(policy, enqueue.entity());
        if sched.placement.on_cpu() == Some(owner) {
            // Fair removes current from its class tree while Linux keeps the
            // task logically on_rq. Re-linking it is put_prev, not activation.
            sched.placement.put_prev(owner);
        } else {
            sched.placement.activate(owner);
        }
        core.set_wake_cpu_hint(owner);
        OwnerReadyEnqueue {
            reschedule,
            scheduler_deadline_refresh_required: enqueue.scheduler_deadline_refresh_required(),
        }
    }

    pub(in crate::system::task_system) fn finish_owner_enqueue(
        &self,
        cpu: Pin<&mut CpuLocal>,
        reason: EnqueueReason,
        reschedule: Option<RescheduleKind>,
        scheduler_deadline_refresh_required: bool,
        effective_policy: Option<SchedulePolicy>,
    ) {
        if reason.checks_preemption_after_enqueue()
            && let Some(kind) = reschedule
        {
            cpu.request_reschedule(kind);
        }
        if let Some(policy) = effective_policy {
            let _started = self.activate_owner_rt_period_for_policy(cpu.owner(), policy);
        }
        if reschedule.is_none()
            && (scheduler_deadline_refresh_required || self.rt_deadline_push_pending(cpu.remote()))
        {
            cpu.remote().kick_scheduler_work();
        }
    }

    pub(in crate::system::task_system) fn activate_owner_deadline_bandwidth(
        &self,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        cpu: Pin<&mut CpuLocal>,
        owner: CpuId,
    ) {
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        Self::activate_deadline_bandwidth_locked(core, sched, &mut transaction, owner);
        transaction.commit();
    }

    pub(in crate::system::task_system) fn activate_deadline_bandwidth_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        run_queue: &mut OwnerRqTxn<'_>,
        owner: CpuId,
    ) {
        if !matches!(sched.policy.base, SchedulePolicy::Deadline(_)) {
            return;
        }
        match sched.deadline.bandwidth.reservation_owner() {
            None => Self::attach_deadline_bandwidth_locked(core, sched, run_queue, owner, true),
            Some(assigned) if assigned != owner => {
                task_runtime::fatal_invariant(0x444c_000a, core.id().as_u64() as usize)
            }
            Some(_) if !sched.deadline.bandwidth.is_active() => {
                run_queue.activate_deadline_bandwidth(sched.deadline.bandwidth.reservation_scaled())
            }
            Some(_) => {}
        }
        sched.deadline.bandwidth.activate_contending();
    }

    /// Attaches one admitted DL reservation to a new rq without changing its
    /// contending state. Linux uses this form for inactive-timer/hotplug
    /// migration, where `this_bw` always moves and `running_bw` moves only for
    /// a still-active reservation.
    pub(in crate::system::task_system) fn attach_deadline_bandwidth_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        run_queue: &mut OwnerRqTxn<'_>,
        owner: CpuId,
        active: bool,
    ) {
        if sched.deadline.bandwidth.reservation_owner().is_some() {
            task_runtime::fatal_invariant(0x444c_0010, core.id().as_u64() as usize);
        }
        run_queue.register_deadline_member(core);
        run_queue.add_deadline_bandwidth(sched.deadline.bandwidth.reservation_scaled(), active);
        sched.deadline.bandwidth.attach(owner);
    }

    pub(in crate::system::task_system) fn detach_owner_deadline_bandwidth_in_rq(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        remote: &CpuRemote,
        run_queue: &mut OwnerRqTxn<'_>,
    ) {
        let owner = remote.owner();
        let Some(assigned_cpu) = sched.deadline.bandwidth.reservation_owner() else {
            return;
        };
        if assigned_cpu != owner {
            task_runtime::fatal_invariant(0x444c_000b, core.id().as_u64() as usize);
        }
        let bandwidth = run_queue.deadline_bandwidth();
        let reservation_scaled = sched.deadline.bandwidth.reservation_scaled();
        if bandwidth.this_bw_scaled() < reservation_scaled
            || (sched.deadline.bandwidth.is_active()
                && bandwidth.running_bw_scaled() < reservation_scaled)
        {
            task_runtime::fatal_invariant(0x444c_000c, core.id().as_u64() as usize);
        }
        Self::cancel_owner_deadline_timers_locked(core, sched, remote);
        run_queue
            .remove_deadline_bandwidth(reservation_scaled, sched.deadline.bandwidth.is_active());
        sched.deadline.bandwidth.detach(owner);
        run_queue.unregister_deadline_member(core);
    }

    pub(in crate::system::task_system) fn mark_owner_deadline_non_contending_in_rq(
        &self,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        mut cpu: Pin<&mut CpuLocal>,
        now_ns: u64,
        run_queue: &mut OwnerRqTxn<'_>,
    ) {
        let owner = cpu.owner();
        let base_entity = if let Some(active) = core.sched().active_option(sched) {
            active.base_entity().clone()
        } else {
            run_queue
                .base_scheduling_entity(core.id())
                .unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x444c_1110, core.id().as_u64() as usize)
                })
        };
        let (Some(assigned_cpu), Some(deadline)) = (
            sched.deadline.bandwidth.reservation_owner(),
            base_entity.deadline(),
        ) else {
            return;
        };
        if assigned_cpu != owner || !sched.deadline.bandwidth.is_contending() {
            return;
        }
        let zero_lag = deadline_zero_lag(deadline);
        let deactivate_now = zero_lag.is_reached_by(SchedulerTimestamp::from_nanos(now_ns));
        if deactivate_now
            && run_queue.deadline_bandwidth().running_bw_scaled()
                < sched.deadline.bandwidth.reservation_scaled()
        {
            task_runtime::fatal_invariant(0x444c_000e, core.id().as_u64() as usize);
        }
        if deactivate_now {
            sched.deadline.bandwidth.deactivate();
        } else {
            sched.deadline.bandwidth.mark_non_contending(zero_lag);
        }
        if self
            .refresh_owner_deadline_timers_in_rq(core, sched, cpu.as_mut(), now_ns, run_queue)
            .is_some()
        {
            cpu.request_scheduler_work();
        }
        if deactivate_now {
            run_queue.deactivate_deadline_bandwidth(sched.deadline.bandwidth.reservation_scaled());
        }
    }
}
