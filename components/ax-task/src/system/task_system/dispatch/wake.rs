//! Direct wakeup transactions.

use super::*;
use crate::{WakePreemptionContext, lock::IrqOwner};

#[derive(Clone, Copy)]
enum WakerCpuSource {
    Current,
}

struct FairWakeContext<'a> {
    affinity: &'a CpuSet,
    waker: Option<CpuId>,
    previous: Option<CpuId>,
    wakee_demand: u64,
    intent: WakeIntent,
    wakee_is_idle: bool,
    wake_wide: bool,
}

struct FairWakeAffineContext {
    waker: CpuId,
    previous: CpuId,
    sync: bool,
    waker_idle: bool,
    previous_idle: bool,
    waker_is_only_runnable: bool,
    waker_demand: u64,
    previous_demand: u64,
}

#[derive(Debug, Eq, PartialEq)]
enum WakeTargetSelection {
    Pinned(CpuId),
    SchedulerClass,
}

/// Mirrors the generic Linux `select_task_rq()` gate before scheduler-class
/// placement runs.
fn wake_target_selection(affinity: &CpuSet) -> WakeTargetSelection {
    affinity.sole_cpu().map_or(
        WakeTargetSelection::SchedulerClass,
        WakeTargetSelection::Pinned,
    )
}

fn select_fair_wake_affine_cpu(context: FairWakeAffineContext) -> CpuId {
    let FairWakeAffineContext {
        waker,
        previous,
        sync,
        waker_idle,
        previous_idle,
        waker_is_only_runnable,
        waker_demand,
        previous_demand,
    } = context;
    if waker_idle {
        return if previous_idle { previous } else { waker };
    }
    if sync && waker_is_only_runnable {
        return waker;
    }
    if previous_idle {
        return previous;
    }
    if waker_demand < previous_demand || (sync && waker_demand == previous_demand) {
        waker
    } else {
        previous
    }
}

struct EqualRtWakeContext<'a> {
    target: CpuId,
    current: &'a CurrentDispatch,
    wakee_policy: SchedulePolicy,
    wakee_affinity: &'a CpuSet,
    reschedule_pending: bool,
}

impl WakerCpuSource {
    fn resolve(self, _activity: &ThreadSchedulerActivity<'_>) -> Option<CpuId> {
        match self {
            Self::Current => {
                // SAFETY: `ThreadSchedulerActivity` owns the wake transaction's
                // preemption guard. If a stronger IRQ or scheduler owner scope
                // supplied that guard, the same scope already retains this CPU.
                let runtime_cpu = unsafe { task_runtime::current_cpu_id() };
                Some(CpuId::new(runtime_cpu.as_u32()))
            }
        }
    }
}

impl TaskSystem {
    fn publish_detached_deadline_owner_work(source_remote: &CpuRemote) -> bool {
        source_remote.kick_scheduler_work()
    }

    fn consume_wake_locked(
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
    ) -> Result<WakeTransition, TaskError> {
        let lifecycle = sched.lifecycle.state();
        if !core.consume_wake(lifecycle == ThreadState::Parking) || lifecycle == ThreadState::Exited
        {
            return Ok(WakeTransition::Notified);
        }
        match lifecycle {
            ThreadState::Parking => Ok(WakeTransition::Notified),
            ThreadState::Blocked => {
                sched.transition(core, ThreadState::Waking)?;
                Ok(WakeTransition::Activate)
            }
            ThreadState::Ready | ThreadState::Running | ThreadState::Waking => {
                Ok(WakeTransition::Notified)
            }
            ThreadState::New | ThreadState::Exited => Ok(WakeTransition::Notified),
        }
    }

    fn select_wake_target(
        &self,
        sched: &ThreadSchedState,
        wakee: &ThreadCore,
        _activity: &ThreadSchedulerActivity<'_>,
        waker: Option<CpuId>,
        previous: Option<CpuId>,
        intent: WakeIntent,
    ) -> Option<CpuId> {
        match wake_target_selection(&sched.affinity.affinity) {
            WakeTargetSelection::Pinned(target) => {
                return self
                    .cpu_remotes
                    .get(target.as_usize())
                    .filter(|remote| remote.accepts_placement())
                    .map(|_| target);
            }
            WakeTargetSelection::SchedulerClass => {}
        }
        let active = wakee.sched().active(sched);
        let policy = active.policy();
        if let SchedulePolicy::Fair { mode, .. } = policy {
            let wake_wide = waker
                .and_then(|_| {
                    let publication = task_runtime::current_thread_publication();
                    // SAFETY: `_activity` pins this execution context until the
                    // synchronous wake transaction returns. Bootstrap contexts
                    // legitimately have no current scheduler thread.
                    unsafe { publication.borrow_current() }.ok()
                })
                .map(|current| {
                    current.runtime_core().record_wakee_and_is_wide(
                        wakee,
                        task_runtime::monotonic_now(),
                        self.root_domain.fair_wake_domain_size(),
                    )
                });
            return self.select_fair_wake_cpu(FairWakeContext {
                affinity: &sched.affinity.affinity,
                waker,
                previous,
                wakee_demand: policy.placement_demand(),
                intent,
                wakee_is_idle: mode == FairMode::Idle,
                wake_wide: wake_wide.unwrap_or(false),
            });
        }
        let preferred = previous.or(waker);
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
        self.select_priority_cpu(
            policy,
            active.entity(),
            &sched.affinity.affinity,
            // Linux enters select_task_rq_{rt,dl} with p->wake_cpu. The
            // current waker is not an implicit placement override for these
            // classes; only Fair wake-affine compares the two CPUs.
            preferred,
            None,
        )
    }

    /// Mirrors Linux `check_preempt_equal_prio()` before mutating the rq FIFO.
    fn equal_rt_wake_action(&self, context: EqualRtWakeContext<'_>) -> EqualRtWakeAction {
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

    fn can_move_rt_from_target(
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
    fn select_fair_idle_sibling(
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
    fn select_fair_wake_cpu(&self, context: FairWakeContext<'_>) -> Option<CpuId> {
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

    /// Wakes a blocked thread from the runtime's current CPU.
    pub(crate) fn wake_thread_from_current_cpu(
        &self,
        core: Arc<ThreadCore>,
        intent: WakeIntent,
    ) -> WakeResult {
        self.wake_thread(core, WakerCpuSource::Current, intent)
    }

    fn wake_thread(
        &self,
        core: Arc<ThreadCore>,
        waker: WakerCpuSource,
        intent: WakeIntent,
    ) -> WakeResult {
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_direct_wake_attempt();
        if core.state() == ThreadState::Exited {
            return WakeResult::Exited;
        }
        let Some(activity) = core.try_scheduler_activity() else {
            return WakeResult::Exited;
        };
        let waker = waker.resolve(&activity);
        let wake_publication = core.publish_wake();

        if wake_publication.already_pending() && wake_publication.state() != ThreadState::Blocked {
            return WakeResult::AlreadyPending;
        }
        match wake_publication.state() {
            ThreadState::Parking
            | ThreadState::Ready
            | ThreadState::Running
            | ThreadState::Waking
            | ThreadState::New => return WakeResult::Notified,
            ThreadState::Exited => {
                core.discard_failed_wake();
                return WakeResult::Exited;
            }
            ThreadState::Blocked => {}
        }

        let mut fail_delivery = false;
        loop {
            let mut sched = core.sched().lock();
            if sched.lifecycle.state() == ThreadState::Exited {
                core.discard_failed_wake();
                return WakeResult::Exited;
            }
            if matches!(
                sched.lifecycle.state(),
                ThreadState::Parking
                    | ThreadState::Ready
                    | ThreadState::Running
                    | ThreadState::Waking
            ) {
                // Parking and its final transition to Blocked are serialized by
                // this task lock, matching Linux try_to_wake_up() under p->pi_lock.
                // If the parker still owns the task, the sticky notification is
                // the complete transaction; otherwise the Blocked path below
                // performs the no-fail runnable publication.
                return WakeResult::Notified;
            }
            // Linux checks `p->on_rq` and runs `ttwu_runnable()` before it
            // waits for `p->on_cpu`. A delayed Fair sleeper deliberately
            // retains rq membership through switch tail, so it reactivates on
            // that rq without taking the ordinary direct-activation path.
            if let Some(target) = sched.placement.queued_cpu() {
                let force_failure = core::mem::take(&mut fail_delivery);
                let publication = (!force_failure)
                    .then(|| self.cpu_remotes[target.as_usize()].begin_publication())
                    .flatten();
                let Some(publication) = publication else {
                    // CPU hotplug closes placement before it takes task locks.
                    // Release p->pi_lock's equivalent so that transaction can
                    // either redirect dormant placement or cancel deactivation,
                    // then retry from a fresh state/on_rq/affinity snapshot.
                    drop(sched);
                    continue;
                };
                let transition =
                    Self::consume_wake_locked(&core, &mut sched).unwrap_or_else(|_| {
                        task_runtime::fatal_invariant(0x574b_000e, core.id().as_u64() as usize)
                    });
                if transition != WakeTransition::Activate {
                    task_runtime::fatal_invariant(0x574b_000f, core.id().as_u64() as usize);
                }

                return self.reactivate_delayed_fair_locked(
                    &core,
                    sched,
                    target,
                    publication,
                    intent,
                );
            }
            let previous = sched
                .placement
                .assigned_cpu()
                .or_else(|| core.wake_cpu_hint());
            let target = self.select_wake_target(&sched, &core, &activity, waker, previous, intent);

            let Some(target) = target else {
                return WakeResult::Unavailable;
            };
            let force_failure = core::mem::take(&mut fail_delivery);
            let publication = (!force_failure)
                .then(|| self.cpu_remotes[target.as_usize()].begin_publication())
                .flatten();
            let Some(publication) = publication else {
                // Target selection raced Online -> Inactive. Do not strand the
                // sticky wake intent and require an unrelated second producer;
                // let hotplug finish its task-lock phase, then reselect exactly
                // as try_to_wake_up() revalidates p->cpu under hotplug.
                drop(sched);
                continue;
            };

            let transition = match Self::consume_wake_locked(&core, &mut sched) {
                Ok(transition) => transition,
                Err(_) => task_runtime::fatal_invariant(0x574b_0002, core.id().as_u64() as usize),
            };
            match transition {
                WakeTransition::Notified => return WakeResult::Notified,
                WakeTransition::Activate => {
                    return self.activate_waking_thread_locked(
                        &core,
                        sched,
                        target,
                        publication,
                        intent,
                    );
                }
            }
        }
    }

    /// Delivers one wait-queue notification to the exact park generation that
    /// published its waiter.
    ///
    /// Selection is owned by the wait-queue lock. This scheduler transaction
    /// publishes `Delivered` only after every recoverable placement step has
    /// succeeded and immediately before the no-fail runnable publication.
    pub(crate) fn wake_wait_claim_from_current_cpu(
        &self,
        core: Arc<ThreadCore>,
        claim: &WaitWakeClaim,
        intent: WakeIntent,
    ) -> WaitWakeDelivery {
        if claim.thread() != core.id() {
            claim.cancel_selected();
            return WaitWakeDelivery::Cancelled;
        }
        if core.state() == ThreadState::Exited {
            claim.cancel_selected();
            return WaitWakeDelivery::Exited;
        }
        let Some(activity) = core.try_scheduler_activity() else {
            claim.cancel_selected();
            return WaitWakeDelivery::Exited;
        };
        let waker = WakerCpuSource::Current.resolve(&activity);
        let mut sched = core.sched().lock();
        if core.park_generation() != claim.park_generation() {
            claim.cancel_selected();
            return WaitWakeDelivery::Cancelled;
        }
        match sched.lifecycle.state() {
            ThreadState::Parking => {
                if !claim.deliver_selected() {
                    return WaitWakeDelivery::Cancelled;
                }

                // The sticky publication normally makes the owner's final
                // park CAS restore Running. The rq-only FIFO/RR block path
                // may win that CAS immediately before this store; the state
                // returned by fetch_or then proves that this waker must finish
                // activation itself instead of leaving a sleeping task with
                // a pending bit.
                let wake = core.publish_wake();
                if wake.state() != ThreadState::Blocked {
                    return WaitWakeDelivery::Delivered;
                }
                // The rq-only FIFO/RR parker does not take this task lock.
                // It reserves detached ownership before publishing Blocked,
                // so a claim which acquired the task lock while the state was
                // still Parking can observe Blocked before `on_rq` removal and
                // detached-owner installation finish. Drop that stale guard
                // and reacquire through the publication-aware lock path. This
                // is the same retry boundary as Linux's on_rq check under
                // p->pi_lock followed by task_rq_lock() revalidation.
                drop(sched);
                let mut sched = core.sched().lock();
                if core.park_generation() != claim.park_generation()
                    || sched.lifecycle.state() != ThreadState::Blocked
                {
                    return WaitWakeDelivery::Delivered;
                }
                let target = sched.placement.assigned_cpu().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x574b_0017, core.id().as_u64() as usize)
                });
                if sched.placement.queued_cpu().is_some() {
                    task_runtime::fatal_invariant(0x574b_0018, core.id().as_u64() as usize);
                }
                let publication = self.cpu_remotes[target.as_usize()]
                    .begin_publication()
                    .unwrap_or_else(|| {
                        task_runtime::fatal_invariant(0x574b_0019, core.id().as_u64() as usize)
                    });
                let transition =
                    Self::consume_wake_locked(&core, &mut sched).unwrap_or_else(|_| {
                        task_runtime::fatal_invariant(0x574b_001a, core.id().as_u64() as usize)
                    });
                if transition != WakeTransition::Activate {
                    task_runtime::fatal_invariant(0x574b_001b, core.id().as_u64() as usize);
                }
                let result =
                    self.activate_waking_thread_locked(&core, sched, target, publication, intent);
                if result != WakeResult::Notified {
                    task_runtime::fatal_invariant(0x574b_001c, core.id().as_u64() as usize);
                }
                WaitWakeDelivery::Delivered
            }
            ThreadState::Blocked => {
                if let Some(target) = sched.placement.queued_cpu() {
                    let Some(publication) = self.cpu_remotes[target.as_usize()].begin_publication()
                    else {
                        claim.cancel_selected();
                        return WaitWakeDelivery::Unavailable;
                    };
                    if !claim.deliver_selected() {
                        return WaitWakeDelivery::Cancelled;
                    }
                    let _already_pending = core.publish_wake();
                    let transition =
                        Self::consume_wake_locked(&core, &mut sched).unwrap_or_else(|_| {
                            task_runtime::fatal_invariant(0x574b_0010, core.id().as_u64() as usize)
                        });
                    if transition != WakeTransition::Activate {
                        task_runtime::fatal_invariant(0x574b_0011, core.id().as_u64() as usize);
                    }
                    let result = self.reactivate_delayed_fair_locked(
                        &core,
                        sched,
                        target,
                        publication,
                        intent,
                    );
                    if result != WakeResult::Notified {
                        task_runtime::fatal_invariant(0x574b_0012, core.id().as_u64() as usize);
                    }
                    return WaitWakeDelivery::Delivered;
                }
                let target = if let Some(target) = sched.placement.committed_migration_target() {
                    // Linux's `task_rq_lock()` waits out
                    // `TASK_ON_RQ_MIGRATING`. The carrier destination is
                    // immutable, so a wake which wins our task lock completes
                    // that exact transfer instead of load-balancing elsewhere.
                    target
                } else {
                    let previous = sched
                        .placement
                        .assigned_cpu()
                        .or_else(|| core.wake_cpu_hint());
                    let Some(target) =
                        self.select_wake_target(&sched, &core, &activity, waker, previous, intent)
                    else {
                        claim.cancel_selected();
                        return WaitWakeDelivery::Unavailable;
                    };
                    target
                };

                let Some(publication) = self.cpu_remotes[target.as_usize()].begin_publication()
                else {
                    claim.cancel_selected();
                    return WaitWakeDelivery::Unavailable;
                };

                if !claim.deliver_selected() {
                    return WaitWakeDelivery::Cancelled;
                }
                let _already_pending = core.publish_wake();
                let transition =
                    Self::consume_wake_locked(&core, &mut sched).unwrap_or_else(|_| {
                        task_runtime::fatal_invariant(0x574b_000b, core.id().as_u64() as usize)
                    });
                if transition != WakeTransition::Activate {
                    task_runtime::fatal_invariant(0x574b_000c, core.id().as_u64() as usize);
                }
                let result =
                    self.activate_waking_thread_locked(&core, sched, target, publication, intent);
                if result != WakeResult::Notified {
                    task_runtime::fatal_invariant(0x574b_000d, core.id().as_u64() as usize);
                }
                WaitWakeDelivery::Delivered
            }
            ThreadState::Exited => {
                claim.cancel_selected();
                WaitWakeDelivery::Exited
            }
            ThreadState::New | ThreadState::Ready | ThreadState::Running | ThreadState::Waking => {
                // Another wake source or a later park generation owns the
                // runnable state. Do not leave a notification for its next
                // park attempt.
                claim.cancel_selected();
                WaitWakeDelivery::Cancelled
            }
        }
    }

    /// Cancels Linux Fair delayed dequeue without waiting for `on_cpu`.
    ///
    /// The task already owns `TASK_ON_RQ_QUEUED`; `ENQUEUE_DELAYED` only
    /// refreshes lag, clears the delayed bit, and evaluates wakeup preemption.
    fn reactivate_delayed_fair_locked(
        &self,
        core: &Arc<ThreadCore>,
        mut sched_guard: crate::lock::IrqTicketGuard<'_, ThreadSchedState>,
        target: CpuId,
        publication: CpuRemotePublication<'_>,
        intent: WakeIntent,
    ) -> WakeResult {
        let (sched, irq_owner) = sched_guard.split_irq_owner();
        if sched.lifecycle.state() != ThreadState::Waking
            || sched.placement.queued_cpu() != Some(target)
        {
            task_runtime::fatal_invariant(0x574b_0013, core.id().as_u64() as usize);
        }
        let remote = &self.cpu_remotes[target.as_usize()];
        remote.cancel_idle_pull_if_uncommitted();
        let mut run_queue = OwnerRqTxn::begin_nested(self, remote, &irq_owner);

        if !run_queue.is_delayed_fair(core.id()) {
            task_runtime::fatal_invariant(0x574b_0014, core.id().as_u64() as usize);
        }
        let policy = run_queue
            .scheduling_state(core.id())
            .map(|(policy, _entity)| policy)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x574b_0016, core.id().as_u64() as usize)
            });
        if matches!(policy, SchedulePolicy::Fair { .. })
            && run_queue.current().is_some_and(|current| {
                matches!(current.schedule_policy(), SchedulePolicy::Fair { .. })
            })
        {
            let _ = run_queue.settle_current(0);
        }
        let current_fair = run_queue.current_fair_contender();

        run_queue.update_fair_virtual_time(current_fair);
        let enqueue = run_queue.reactivate_delayed_fair(
            core.id(),
            current_fair,
            self.config.timing_granularity_ns(),
        );

        run_queue.update_fair_virtual_time(current_fair);
        let fair_virtual_time = enqueue
            .entity()
            .fair()
            .map_or(0, |_| run_queue.virtual_time());

        let reschedule_pending = remote.immediate_preemption_requested();
        let preemption = run_queue.wakeup_preempt_with_intent(
            core.id(),
            policy,
            enqueue.entity(),
            fair_virtual_time,
            WakePreemptionContext::new(
                intent,
                EqualRtWakeAction::PreserveFifoOrder,
                reschedule_pending,
            ),
        );

        let reschedule = preemption.reschedule_kind(policy);

        core.publish_effective_schedule(policy, enqueue.entity());
        core.set_wake_cpu_hint(target);
        if sched.transition(core, ThreadState::Running).is_err() {
            task_runtime::fatal_invariant(0x574b_0015, core.id().as_u64() as usize);
        }
        let owner_work_required = enqueue.scheduler_deadline_refresh_required();
        run_queue.commit();
        drop(sched_guard);
        drop(publication);
        match (reschedule, owner_work_required) {
            (Some(kind), true) => remote.request_remote_reschedule_with_scheduler_work(kind),
            (Some(kind), false) => remote.request_remote_reschedule(kind),
            (None, true) => {
                remote.kick_scheduler_work();
            }
            (None, false) => {}
        }
        WakeResult::Notified
    }

    pub(in crate::system::task_system) fn activate_waking_thread_locked(
        &self,
        core: &Arc<ThreadCore>,
        mut sched_guard: crate::lock::IrqTicketGuard<'_, ThreadSchedState>,
        target: CpuId,
        publication: CpuRemotePublication<'_>,
        intent: WakeIntent,
    ) -> WakeResult {
        // PREEMPT_RT disables TTWU_QUEUE. The waker therefore retains the task
        // lock, pairs this acquire wait with `finish_task()`'s release-clear of
        // `on_cpu`, then activates the task under its selected rq lock.

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

        let now_ns = run_queue.clock().wall().as_nanos();
        let mut active = core.sched().active(sched);
        let policy = active.policy();
        if matches!(policy, SchedulePolicy::Fair { .. })
            && run_queue.current().is_some_and(|current| {
                matches!(current.schedule_policy(), SchedulePolicy::Fair { .. })
            })
        {
            let _ = run_queue.settle_current(0);
        }
        let mut queued_entity = active.entity().clone();
        let deadline_wake = matches!(policy, SchedulePolicy::Deadline(_)) && !sched.is_pi_boosted();
        if deadline_wake {
            queued_entity.activate_deadline(now_ns);
            *active.entity_mut() = queued_entity.clone();
        }
        drop(active);
        Self::activate_deadline_bandwidth_locked(core, sched, &mut run_queue, target);
        if deadline_wake
            && queued_entity
                .deadline()
                .is_some_and(DeadlineEntity::is_throttled)
        {
            self.link_owner_throttled_deadline_locked(&mut run_queue, core, sched, target);
            if sched.transition(core, ThreadState::Running).is_err() {
                task_runtime::fatal_invariant(0x574b_0006, core.id().as_u64() as usize);
            }
            #[cfg(feature = "qperf-metrics")]
            crate::metrics::record_direct_wake_activation();
            run_queue.commit();
            drop(sched_guard);
            self.publish_owner_deadline_refresh_reserved(core, target, publication);
            return WakeResult::Notified;
        }
        let maintains_fair_virtual_time = queued_entity.fair().is_some();
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
        debug_assert_eq!(active.entity(), &queued_entity);
        let delayed_migration_wake = queued_entity
            .fair()
            .is_some_and(|fair| fair.is_delayed_migrating());
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

        let preempts_current = reschedule.is_some();
        core.publish_effective_schedule(policy, enqueue.entity());
        core.set_wake_cpu_hint(target);
        if sched.transition(core, ThreadState::Running).is_err() {
            task_runtime::fatal_invariant(0x574b_0006, core.id().as_u64() as usize);
        }
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_direct_wake_activation();
        let push_class = super::super::balance::push_class_for_policy(policy)
            .filter(|class| run_queue.has_pushable_class_tasks(class.scheduling_class()));
        run_queue.commit();
        drop(sched_guard);
        let rt_period_started = self.activate_owner_rt_period_for_policy(target, policy);
        // Linux publishes wake preemption, RT/DL push work, and a newly active
        // bandwidth period after the enqueue transaction. Preserve each
        // logical reason while coalescing their shared physical delivery.
        let owner_work_required =
            enqueue.scheduler_deadline_refresh_required() || rt_period_started;

        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_direct_wake_enqueue();
        #[cfg(feature = "qperf-metrics")]
        if preempts_current {
            crate::metrics::record_direct_wake_preemption();
        }
        #[cfg(feature = "qperf-metrics")]
        match preemption {
            WakePreemptionDecision::KeepCurrent => {
                crate::metrics::record_direct_wake_current_kept()
            }
            WakePreemptionDecision::DedicatedIdlePreempted => {}
            WakePreemptionDecision::QueuedCandidateSelected => {
                crate::metrics::record_direct_wake_queued_candidate_selected()
            }
            WakePreemptionDecision::WakeeSelected => {}
        }
        if deadline_wake {
            if preempts_current {
                remote.request_reschedule(RescheduleKind::Immediate);
            }
            // The reserved Deadline refresh is already an owner-control
            // publication. Its scheduler-work bit covers RT/DL push and a new
            // root-period projection, so a second generation is redundant.
            self.publish_owner_deadline_refresh_reserved(core, target, publication);
        } else {
            drop(publication);
            match (reschedule, owner_work_required) {
                (Some(kind), true) => remote.request_remote_reschedule_with_scheduler_work(kind),
                (Some(kind), false) => remote.request_remote_reschedule(kind),
                (None, true) => {
                    remote.kick_scheduler_work();
                }
                (None, false) => {}
            }
        }
        if let Some(class) = push_class {
            self.root_domain.start_rt_deadline_push_from(class, target);
        }
        WakeResult::Notified
    }

    pub(in crate::system::task_system) fn enqueue_owner_thread(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: Arc<ThreadCore>,
        reason: EnqueueReason,
    ) -> Result<(), TaskError> {
        self.ensure_owner_cpu_online(&cpu)?;
        let mut sched_guard = core.sched().lock();
        let (sched, irq_owner) = sched_guard.split_irq_owner();
        let commit =
            self.enqueue_owner_thread_locked(cpu.as_mut(), &core, sched, &irq_owner, reason)?;
        let affinity_completed = Self::complete_affinity_if_satisfied_locked(&core, sched);
        drop(sched_guard);
        if affinity_completed {
            core.notify_affinity_waiters();
        }
        self.finish_owner_enqueue(
            cpu,
            reason,
            commit.reschedule,
            commit.scheduler_deadline_refresh_required,
            Some(commit.effective_policy),
            commit.push_class,
        );
        Ok(())
    }

    fn enqueue_owner_thread_locked(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        irq_owner: &IrqOwner<'_>,
        reason: EnqueueReason,
    ) -> Result<OwnerEnqueueCommit, TaskError> {
        let owner = cpu.owner();
        if sched.lifecycle.state() != ThreadState::Running {
            return Err(TaskError::NotReady);
        }
        if !sched.affinity.affinity.contains(owner) && !matches!(reason, EnqueueReason::Migrated) {
            return Err(TaskError::InvalidCpu(owner.as_u32()));
        }
        cpu.as_ref()
            .get_ref()
            .remote()
            .cancel_idle_pull_if_uncommitted();
        let remote = Arc::clone(cpu.remote());
        let mut transaction = OwnerRqTxn::begin_nested(self, &remote, irq_owner);

        let now_ns = transaction.clock().wall().as_nanos();
        let mut active = core.sched().active(sched);
        let policy = active.policy();
        let mut queued_entity = active.entity().clone();
        if matches!(reason, EnqueueReason::Wake)
            && matches!(policy, SchedulePolicy::Deadline(_))
            && !sched.is_pi_boosted()
        {
            queued_entity.activate_deadline(now_ns);
            *active.entity_mut() = queued_entity.clone();
        }
        drop(active);
        let deadline_wake_throttled = queued_entity
            .deadline()
            .is_some_and(DeadlineEntity::is_throttled);
        if deadline_wake_throttled {
            self.link_owner_throttled_deadline_locked(&mut transaction, core, sched, owner);
            let preempts_current = self
                .refresh_owner_deadline_timers_in_rq(
                    core,
                    sched,
                    cpu.as_mut(),
                    now_ns,
                    &mut transaction,
                )
                .unwrap_or(false);
            transaction.commit();
            return Ok(OwnerEnqueueCommit {
                reschedule: preempts_current.then_some(RescheduleKind::Immediate),
                scheduler_deadline_refresh_required: false,
                effective_policy: policy,
                push_class: None,
            });
        }
        if reason.checks_preemption_after_enqueue()
            && queued_entity.fair().is_some()
            && transaction.current_fair_contender().is_some()
        {
            // Linux `enqueue_entity()` executes `update_curr()` before it
            // places the wakee and `wakeup_preempt_fair()` compares EEVDF
            // state. Owner-local ready placement does not pass through the
            // direct-wake settlement path, so it must close the same runtime
            // interval here.
            let _ = transaction.settle_current(0);
        }
        let enqueue =
            self.link_owner_ready_thread_locked(owner, &mut transaction, core, sched, reason);
        let timer_preempts = self
            .refresh_owner_deadline_timers_in_rq(core, sched, cpu, now_ns, &mut transaction)
            .unwrap_or(false);
        let push_class = super::super::balance::push_class_for_policy(policy)
            .filter(|class| transaction.has_pushable_class_tasks(class.scheduling_class()));
        transaction.commit();
        Ok(OwnerEnqueueCommit {
            reschedule: if timer_preempts {
                Some(RescheduleKind::Immediate)
            } else {
                enqueue.reschedule
            },
            scheduler_deadline_refresh_required: enqueue.scheduler_deadline_refresh_required,
            effective_policy: policy,
            push_class,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_sync_wake_affine_prefers_singleton_waker_rq() {
        let waker = CpuId::new(0);
        let previous = CpuId::new(1);

        assert_eq!(
            select_fair_wake_affine_cpu(FairWakeAffineContext {
                waker,
                previous,
                sync: true,
                waker_idle: false,
                previous_idle: false,
                waker_is_only_runnable: true,
                waker_demand: 2_048,
                previous_demand: 1_024,
            }),
            waker,
        );
    }

    #[test]
    fn linux_singleton_affinity_skips_scheduler_class_wake_selection() {
        let pinned = CpuId::new(2);
        let mut affinity = CpuSet::empty(4);
        assert!(affinity.insert(pinned));

        assert_eq!(
            wake_target_selection(&affinity),
            WakeTargetSelection::Pinned(pinned),
        );
    }
}
