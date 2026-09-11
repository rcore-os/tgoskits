//! Placement under the owning scheduler transaction.

use super::*;

impl TaskSystem {
    pub(super) fn select_wake_target(
        &self,
        sched: &ThreadSchedState,
        wakee: &ThreadCore,
        waker: Option<CpuId>,
        previous: Option<CpuId>,
        intent: WakeIntent,
    ) -> Option<CpuId> {
        match wake_target_selection(&sched.affinity.affinity) {
            WakeTargetSelection::Pinned(target) => {
                return self
                    .cpu_remotes
                    .get(target.as_usize())
                    .filter(|remote| {
                        // Linux is_cpu_allowed() keeps per-CPU kthreads on an
                        // online but inactive CPU until hotplug parks them.
                        // Only the registered timer worker may finish soft
                        // work here; ordinary pinned tasks need active placement.
                        remote.accepts_placement()
                            || (remote.is_online() && remote.ktimer_worker() == Some(wakee.id()))
                    })
                    .map(|_| target);
            }
            WakeTargetSelection::SchedulerClass => {}
        }
        let policy = wakee.effective_policy_snapshot();
        if let SchedulePolicy::Fair { mode, .. } = policy {
            let waker = waker.or_else(|| {
                Some(CpuId::new(unsafe {
                    task_runtime::current_cpu_id().as_u32()
                }))
            });
            let wake_wide = {
                let publication = task_runtime::current_thread_publication();
                // SAFETY: the preempt scope pins this execution context until
                // the synchronous wake transaction returns. Bootstrap
                // contexts legitimately have no current scheduler thread.
                unsafe { publication.borrow_current() }
                    .ok()
                    .is_some_and(|current| {
                        current.runtime_core().record_wakee_and_is_wide(
                            wakee,
                            task_runtime::monotonic_now(),
                            self.root_domain.fair_wake_domain_size(),
                        )
                    })
            };
            return self.select_fair_wake_cpu(FairWakeContext {
                affinity: &sched.affinity.affinity,
                waker,
                previous,
                wakee_demand: policy.placement_demand(),
                intent,
                wakee_is_idle: mode == FairMode::Idle,
                wake_wide,
            });
        }
        let preferred = previous.or_else(|| {
            waker.or_else(|| {
                Some(CpuId::new(unsafe {
                    task_runtime::current_cpu_id().as_u32()
                }))
            })
        });
        if let Some(priority) = policy.rt_priority()
            && let Some(previous) = preferred
            && sched.affinity.affinity.contains(previous)
            && self
                .cpu_remotes
                .get(previous.as_usize())
                .is_some_and(|remote| {
                    remote.accepts_placement() && !remote.rt_wake_requires_cpupri(priority)
                })
        {
            // Linux keeps a higher-priority wakee cache-hot on its previous
            // rq. The lower-priority donor is pushed after preemption instead
            // of bouncing the wakee merely because another CPU is idle.
            return Some(previous);
        }
        let entity =
            matches!(policy, SchedulePolicy::Deadline(_)).then(|| wakee.sched().active(sched));
        self.select_priority_cpu(
            policy,
            entity.as_ref().map(|active| active.entity()),
            &sched.affinity.affinity,
            // Linux enters select_task_rq_{rt,dl} with p->wake_cpu. The
            // current waker is not an implicit placement override for these
            // classes; only Fair wake-affine compares the two CPUs.
            preferred,
            None,
        )
    }

    /// Mirrors Linux `check_preempt_equal_prio()` before mutating the rq FIFO.
    pub(super) fn equal_rt_wake_action(
        &self,
        context: EqualRtWakeContext<'_>,
    ) -> EqualRtWakeAction {
        let Some(wakee_priority) = context.wakee_policy.rt_priority() else {
            return EqualRtWakeAction::PreserveFifoOrder;
        };
        let current_policy = context.current.schedule_policy();
        if context.reschedule_pending || current_policy.rt_priority() != Some(wakee_priority) {
            return EqualRtWakeAction::PreserveFifoOrder;
        }
        let current_affinity = &context.current.metadata().affinity;
        if !current_affinity.is_migration_capable()
            || !self.can_move_rt_from_target(current_policy, current_affinity, context.target)
        {
            return EqualRtWakeAction::PreserveFifoOrder;
        }

        if context.wakee_affinity.is_migration_capable()
            && self.can_move_rt_from_target(
                context.wakee_policy,
                context.wakee_affinity,
                context.target,
            )
        {
            return EqualRtWakeAction::PreserveFifoOrder;
        }
        EqualRtWakeAction::RequeueWakeeAndReschedule
    }

    pub(super) fn can_move_rt_from_target(
        &self,
        policy: SchedulePolicy,
        affinity: &CpuSet,
        target: CpuId,
    ) -> bool {
        let Some(priority) = policy.rt_priority() else {
            return false;
        };
        let accepts = |cpu: CpuId| {
            cpu != target
                && self
                    .cpu_remotes
                    .get(cpu.as_usize())
                    .is_some_and(|remote| remote.accepts_placement() && remote.is_scheduler_ready())
        };
        self.root_domain
            .find_lowest_rt_cpu(priority, affinity, None, accepts)
            .is_some()
    }

    /// Mirrors Linux `select_idle_sibling()` for the current flat root domain.
    ///
    /// Linux first tests the wake-affine target, then the previous CPU, then
    /// scans their LLC domain. ArceOS does not publish cache or capacity
    /// topology yet, so every eligible CPU in the root domain is a sibling.
    /// An incoming migration reservation makes an otherwise empty rq busy:
    /// another wake transaction has already selected that CPU.
    pub(super) fn select_fair_idle_sibling(
        &self,
        affinity: &CpuSet,
        previous: Option<CpuId>,
        target: CpuId,
        wakee_is_idle: bool,
    ) -> CpuId {
        let is_idle = |cpu: CpuId| {
            affinity.contains(cpu)
                && self.cpu_remotes.get(cpu.as_usize()).is_some_and(|remote| {
                    remote.accepts_placement()
                        && remote.is_scheduler_ready()
                        && remote.is_fair_idle_placement_target(wakee_is_idle)
                })
        };
        if is_idle(target) {
            return target;
        }
        if let Some(previous) = previous.filter(|previous| *previous != target)
            && is_idle(previous)
        {
            return previous;
        }
        affinity
            .iter()
            .find(|cpu| *cpu != target && Some(*cpu) != previous && is_idle(*cpu))
            .unwrap_or(target)
    }

    /// Mirrors Linux Fair `select_task_rq_fair()` for a blocked wake.
    ///
    /// Wake-affine first compares the post-wake demand on the waking and
    /// previous CPUs. Linux's PELT `cpu_load(previous)` still includes a
    /// blocked wakee, so `wake_affine_weight()` removes that contribution from
    /// the previous candidate and adds it to the waker candidate. This
    /// instantaneous model excludes blocked tasks already: leave the previous
    /// demand unchanged and add the wakee only to the waker candidate. Linux
    /// then invokes `select_idle_sibling()` for `WF_TTWU`; omitting that second
    /// stage stacks wakees on busy CPUs while siblings remain idle. For
    /// `WF_SYNC`, wake-affine also discounts the current waker and biases a
    /// load tie toward that CPU before the same idle-sibling stage.
    pub(super) fn select_fair_wake_cpu(&self, context: FairWakeContext<'_>) -> Option<CpuId> {
        let FairWakeContext {
            affinity,
            waker,
            previous,
            wakee_demand,
            intent,
            wakee_is_idle,
            wake_wide,
        } = context;
        let eligible = |cpu: CpuId| {
            affinity.contains(cpu)
                && self
                    .cpu_remotes
                    .get(cpu.as_usize())
                    .is_some_and(|remote| remote.accepts_placement())
        };
        let waker = waker.filter(|cpu| eligible(*cpu));
        let previous = previous.filter(|cpu| eligible(*cpu));
        let target = match (waker, previous) {
            (_, Some(previous)) if wake_wide => Some(previous),
            (Some(waker), Some(previous)) if waker != previous => {
                let waker_remote = &self.cpu_remotes[waker.as_usize()];
                let waker_demand = if intent.is_sync() {
                    waker_remote.sync_wake_affine_demand()
                } else {
                    waker_remote.placement_demand()
                }
                .saturating_add(wakee_demand);
                let previous_remote = &self.cpu_remotes[previous.as_usize()];
                let previous_demand = previous_remote.placement_demand();
                let waker_idle = waker_remote.is_fair_idle_placement_target(wakee_is_idle);
                let previous_idle = previous_remote.is_fair_idle_placement_target(wakee_is_idle);
                let waker_is_only_runnable = waker_remote.sync_wake_affine_is_singleton();
                Some(select_fair_wake_affine_cpu(FairWakeAffineContext {
                    waker,
                    previous,
                    sync: intent.is_sync(),
                    waker_idle,
                    previous_idle,
                    waker_is_only_runnable,
                    waker_demand,
                    previous_demand,
                }))
            }
            (Some(cpu), _) | (_, Some(cpu)) => Some(cpu),
            (None, None) => self.select_fair_active_cpu(affinity, None),
        }?;
        Some(self.select_fair_idle_sibling(affinity, previous, target, wakee_is_idle))
    }
}
