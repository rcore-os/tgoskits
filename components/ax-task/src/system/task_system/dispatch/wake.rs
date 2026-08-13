//! Direct wakeup and owner-runqueue activation transactions.

use super::*;

#[cfg(test)]
static WAKE_BEFORE_THREAD_LOCK_RACE_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static WAKE_BEFORE_THREAD_LOCK_RACE_SYSTEM: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static WAKE_BEFORE_THREAD_LOCK_RACE_THREAD: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static WAKE_BEFORE_THREAD_LOCK_RACE_ENTERED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static WAKE_BEFORE_THREAD_LOCK_RACE_COMPLETED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
static WAKE_DURING_FINAL_PARK_PUBLICATION_ARMED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static WAKE_DURING_FINAL_PARK_PUBLICATION_SYSTEM: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static WAKE_DURING_FINAL_PARK_PUBLICATION_THREAD: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static WAKE_DURING_FINAL_PARK_PUBLICATION_ENTERED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
static WAKE_DURING_FINAL_PARK_PUBLICATION_COMPLETED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[derive(Clone, Copy)]
enum WakerCpuSource {
    Current,
    #[cfg(test)]
    Explicit(Option<CpuId>),
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
            #[cfg(test)]
            Self::Explicit(waker) => waker,
        }
    }
}

#[cfg(test)]
pub(in crate::system::task_system) fn arm_wake_before_thread_lock_race(
    system: &TaskSystem,
    thread: ThreadId,
) {
    use core::sync::atomic::Ordering;

    WAKE_BEFORE_THREAD_LOCK_RACE_ENTERED.store(false, Ordering::Release);
    WAKE_BEFORE_THREAD_LOCK_RACE_COMPLETED.store(false, Ordering::Release);
    assert!(
        !WAKE_BEFORE_THREAD_LOCK_RACE_ARMED.swap(true, Ordering::AcqRel),
        "only one deterministic pre-lock wake race may be armed"
    );
    WAKE_BEFORE_THREAD_LOCK_RACE_SYSTEM.store(
        (system as *const TaskSystem).expose_provenance(),
        Ordering::Release,
    );
    WAKE_BEFORE_THREAD_LOCK_RACE_THREAD.store(thread.as_u64(), Ordering::Release);
}

#[cfg(test)]
pub(in crate::system::task_system) fn wake_before_thread_lock_race_entered() -> bool {
    WAKE_BEFORE_THREAD_LOCK_RACE_ENTERED.load(core::sync::atomic::Ordering::Acquire)
}

#[cfg(test)]
pub(in crate::system::task_system) fn complete_wake_before_thread_lock_race() {
    WAKE_BEFORE_THREAD_LOCK_RACE_COMPLETED.store(true, core::sync::atomic::Ordering::Release);
}

#[cfg(test)]
fn wake_before_thread_lock_race_hook(system: &TaskSystem, thread: ThreadId) {
    use core::sync::atomic::Ordering;

    if WAKE_BEFORE_THREAD_LOCK_RACE_SYSTEM.load(Ordering::Acquire)
        != (system as *const TaskSystem).expose_provenance()
        || WAKE_BEFORE_THREAD_LOCK_RACE_THREAD.load(Ordering::Acquire) != thread.as_u64()
    {
        return;
    }
    if !WAKE_BEFORE_THREAD_LOCK_RACE_ARMED.swap(false, Ordering::AcqRel) {
        return;
    }
    WAKE_BEFORE_THREAD_LOCK_RACE_ENTERED.store(true, Ordering::Release);
    while !WAKE_BEFORE_THREAD_LOCK_RACE_COMPLETED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
}

#[cfg(test)]
pub(in crate::system::task_system) fn arm_wake_during_final_park_publication(
    system: &TaskSystem,
    thread: ThreadId,
) {
    use core::sync::atomic::Ordering;

    WAKE_DURING_FINAL_PARK_PUBLICATION_ENTERED.store(false, Ordering::Release);
    WAKE_DURING_FINAL_PARK_PUBLICATION_COMPLETED.store(false, Ordering::Release);
    assert!(
        !WAKE_DURING_FINAL_PARK_PUBLICATION_ARMED.swap(true, Ordering::AcqRel),
        "only one deterministic final-park wake race may be armed"
    );
    WAKE_DURING_FINAL_PARK_PUBLICATION_SYSTEM.store(
        (system as *const TaskSystem).expose_provenance(),
        Ordering::Release,
    );
    WAKE_DURING_FINAL_PARK_PUBLICATION_THREAD.store(thread.as_u64(), Ordering::Release);
}

#[cfg(test)]
pub(in crate::system::task_system) fn wake_during_final_park_publication_entered() -> bool {
    WAKE_DURING_FINAL_PARK_PUBLICATION_ENTERED.load(core::sync::atomic::Ordering::Acquire)
}

#[cfg(test)]
pub(in crate::system::task_system) fn complete_wake_during_final_park_publication() {
    WAKE_DURING_FINAL_PARK_PUBLICATION_COMPLETED.store(true, core::sync::atomic::Ordering::Release);
}

#[cfg(test)]
fn wake_during_final_park_publication_hook(system: &TaskSystem, thread: ThreadId) {
    use core::sync::atomic::Ordering;

    if WAKE_DURING_FINAL_PARK_PUBLICATION_SYSTEM.load(Ordering::Acquire)
        != (system as *const TaskSystem).expose_provenance()
        || WAKE_DURING_FINAL_PARK_PUBLICATION_THREAD.load(Ordering::Acquire) != thread.as_u64()
    {
        return;
    }
    if !WAKE_DURING_FINAL_PARK_PUBLICATION_ARMED.swap(false, Ordering::AcqRel) {
        return;
    }
    WAKE_DURING_FINAL_PARK_PUBLICATION_ENTERED.store(true, Ordering::Release);
    while !WAKE_DURING_FINAL_PARK_PUBLICATION_COMPLETED.load(Ordering::Acquire) {
        core::hint::spin_loop();
    }
}

impl TaskSystem {
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
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        waker: Option<CpuId>,
        previous: Option<CpuId>,
    ) -> Option<CpuId> {
        #[cfg(test)]
        WAKE_TARGET_SELECTIONS.set(WAKE_TARGET_SELECTIONS.get().saturating_add(1));
        if matches!(policy, SchedulePolicy::Fair { .. }) {
            return self.select_fair_wake_cpu(&sched.affinity.affinity, waker, previous);
        }
        self.select_priority_cpu(
            policy,
            entity,
            &sched.affinity.affinity,
            // Linux enters select_task_rq_{rt,dl} with p->wake_cpu. The
            // current waker is not an implicit placement override for these
            // classes; only Fair wake-affine compares the two CPUs.
            previous.or(waker),
            None,
        )
    }

    /// Selects between the waking and previous CPU using Linux wake-affine's
    /// conservative load rule.
    ///
    /// This scheduler currently exposes one flat root domain and no cache or
    /// capacity topology. Moving a wakee to the waker is therefore justified
    /// only when the waker owns strictly less instantaneous demand; a tie
    /// preserves the previous CPU's cache locality. The wake transaction
    /// samples the waker identity under its activity guard but does not fold
    /// that identity into placement ownership.
    fn select_fair_wake_cpu(
        &self,
        affinity: &CpuSet,
        waker: Option<CpuId>,
        previous: Option<CpuId>,
    ) -> Option<CpuId> {
        let eligible = |cpu: CpuId| {
            affinity.contains(cpu)
                && self
                    .cpu_remotes
                    .get(cpu.as_usize())
                    .is_some_and(|remote| remote.accepts_placement())
        };
        let waker = waker.filter(|cpu| eligible(*cpu));
        let previous = previous.filter(|cpu| eligible(*cpu));
        match (waker, previous) {
            (Some(waker), Some(previous)) if waker != previous => {
                let waker_demand = self.cpu_remotes[waker.as_usize()].placement_demand();
                let previous_demand = self.cpu_remotes[previous.as_usize()].placement_demand();
                if waker_demand < previous_demand {
                    Some(waker)
                } else {
                    Some(previous)
                }
            }
            (Some(cpu), _) | (_, Some(cpu)) => Some(cpu),
            (None, None) => self.select_fair_active_cpu(affinity, None),
        }
    }

    /// Activates a blocked thread directly under its target runqueue lock.
    ///
    /// Lock order is thread scheduler state, then target runqueue. This is the
    /// active PREEMPT_RT wakeup model: no owner inbox or later safe point owns
    /// the transition from blocked to physically queued.
    #[cfg(test)]
    pub(crate) fn wake_thread_direct(
        &self,
        core: Arc<ThreadCore>,
        waker: Option<CpuId>,
    ) -> WakeResult {
        self.wake_thread(core, WakerCpuSource::Explicit(waker))
    }

    pub(crate) fn wake_thread_from_current_cpu(&self, core: Arc<ThreadCore>) -> WakeResult {
        self.wake_thread(core, WakerCpuSource::Current)
    }

    fn wake_thread(&self, core: Arc<ThreadCore>, waker: WakerCpuSource) -> WakeResult {
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_direct_wake_attempt();
        if core.state() == ThreadState::Exited {
            return WakeResult::Exited;
        }
        let Some(activity) = core.try_scheduler_activity() else {
            return WakeResult::Exited;
        };
        let waker = waker.resolve(&activity);
        #[cfg(test)]
        wake_during_final_park_publication_hook(self, core.id());
        #[cfg(test)]
        wake_before_thread_lock_race_hook(self, core.id());
        let mut sched = core.sched().lock();
        if sched.lifecycle.state() == ThreadState::Exited {
            return WakeResult::Exited;
        }
        // Serialize publication with lifecycle and placement just as Linux
        // serializes try_to_wake_up() with p->pi_lock. A failed target lookup
        // may clear only the wake owned by this transaction; a concurrent
        // waker cannot observe and coalesce with it until that decision ends.
        if core.publish_wake() {
            return WakeResult::AlreadyPending;
        }
        if matches!(
            sched.lifecycle.state(),
            ThreadState::Parking | ThreadState::Ready | ThreadState::Running | ThreadState::Waking
        ) {
            // Parking and its final transition to Blocked are serialized by
            // this task lock, matching Linux try_to_wake_up() under p->pi_lock.
            // If the parker still owns the task, the sticky notification is
            // the complete transaction; otherwise the Blocked path below
            // performs the no-fail runnable publication.
            return WakeResult::Notified;
        }
        let previous = sched
            .placement
            .assigned_cpu()
            .or_else(|| core.wake_cpu_hint());
        let policy = sched.policy.active().policy();
        let queued_entity = sched.policy.active().entity().clone();
        let target = self.select_wake_target(&sched, policy, queued_entity, waker, previous);
        let Some(target) = target else {
            core.discard_failed_wake();
            return WakeResult::Unavailable;
        };
        let Some(publication) = self.cpu_remotes[target.as_usize()].begin_publication() else {
            core.discard_failed_wake();
            return WakeResult::Unavailable;
        };
        let transition = match Self::consume_wake_locked(&core, &mut sched) {
            Ok(transition) => transition,
            Err(_) => task_runtime::fatal_invariant(0x574b_0002, core.id().as_u64() as usize),
        };
        match transition {
            WakeTransition::Notified => WakeResult::Notified,
            WakeTransition::Activate => {
                self.activate_waking_thread_locked(&core, sched, target, publication)
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
                // No rq placement is required while the owner is still in the
                // park transaction. Publishing the sticky park bit is the
                // final, infallible step that prevents Blocked publication.
                let _already_pending = core.publish_wake();
                WaitWakeDelivery::Delivered
            }
            ThreadState::Blocked => {
                let previous = sched
                    .placement
                    .assigned_cpu()
                    .or_else(|| core.wake_cpu_hint());
                let policy = sched.policy.active().policy();
                let queued_entity = sched.policy.active().entity().clone();
                let Some(target) =
                    self.select_wake_target(&sched, policy, queued_entity, waker, previous)
                else {
                    claim.cancel_selected();
                    return WaitWakeDelivery::Unavailable;
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
                let result = self.activate_waking_thread_locked(&core, sched, target, publication);
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

    fn activate_waking_thread_locked(
        &self,
        core: &Arc<ThreadCore>,
        mut sched_guard: crate::lock::IrqTicketGuard<'_, ThreadSchedState>,
        target: CpuId,
        publication: CpuRemotePublication<'_>,
    ) -> WakeResult {
        // PREEMPT_RT keeps wake ownership in the waker. `finish_task()` only
        // release-publishes that the old stack is inactive; switch tail never
        // reopens the task lock to finish this wake or enqueues on its behalf.
        #[cfg(feature = "task-test-hooks")]
        let sched_owns_runtime_irq = sched_guard.owns_runtime_irq_scope();
        let (sched, irq_owner) = sched_guard.split_irq_owner();
        sched.placement.wait_until_not_on_cpu();
        if sched.lifecycle.state() != ThreadState::Waking || sched.placement.on_cpu().is_some() {
            task_runtime::fatal_invariant(0x574b_0005, core.id().as_u64() as usize);
        }
        if sched.transition(core, ThreadState::Ready).is_err() {
            task_runtime::fatal_invariant(0x574b_0006, core.id().as_u64() as usize);
        }
        #[cfg(feature = "qperf-metrics")]
        crate::metrics::record_direct_wake_activation();

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
            source_remote.request_scheduler_work();
            source_remote.kick_scheduler_work();
        }
        let mut run_queue = OwnerRqTxn::begin_nested(self, remote, &irq_owner);
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_wake_irq_owner_scopes(
            core.id(),
            sched_owns_runtime_irq,
            run_queue.owns_runtime_irq_scope(),
        );
        let now_ns = run_queue.clock().wall().as_nanos();
        let policy = sched.policy.active().policy();
        let mut queued_entity = sched.policy.active().entity().clone();
        let deadline_wake = matches!(policy, SchedulePolicy::Deadline(_)) && !sched.is_pi_boosted();
        if deadline_wake {
            queued_entity.activate_deadline(now_ns);
            *sched.policy.active_mut().entity_mut() = queued_entity.clone();
        }
        Self::activate_deadline_bandwidth_locked(core, sched, &mut run_queue, target);
        if deadline_wake
            && queued_entity
                .deadline()
                .is_some_and(DeadlineEntity::is_throttled)
        {
            self.link_owner_throttled_deadline_locked(&mut run_queue, core, sched, target);
            run_queue.commit();
            drop(sched_guard);
            self.publish_owner_deadline_refresh_reserved(core, target, publication);
            return WakeResult::Notified;
        }
        let current_fair = run_queue
            .current_scheduling_entity()
            .and_then(|entity| entity.fair());
        run_queue.update_fair_virtual_time(current_fair);
        let metadata = sched.rq_task_metadata().unwrap_or_else(|_| {
            task_runtime::fatal_invariant(0x574b_0103, core.id().as_u64() as usize)
        });
        let active = sched.policy.take_active();
        debug_assert_eq!(active.policy(), policy);
        debug_assert_eq!(active.entity(), &queued_entity);
        let queued_entity = run_queue.enqueue_task(
            QueuedThread::new(
                core.id(),
                active,
                Arc::clone(core),
                sched.is_pi_boosted_rt_owner_for(policy),
                sched.affinity.affinity.is_migration_capable(),
                metadata,
            ),
            EnqueueReason::Wake,
            current_fair,
        );
        run_queue.update_fair_virtual_time(current_fair);
        let fair_virtual_time = queued_entity
            .fair()
            .map_or(0, |fair| run_queue.virtual_time_for_mode(fair.mode()));
        let preemption =
            run_queue.wakeup_preempt(core.id(), policy, queued_entity.clone(), fair_virtual_time);
        let preempts_current = preemption.requests_reschedule();
        core.publish_effective_schedule(policy, &queued_entity);
        sched.placement.activate(target);
        core.set_wake_cpu_hint(target);
        let rt_deadline_push_pending = self.rt_deadline_push_pending(remote);
        run_queue.commit();
        drop(sched_guard);
        let rt_period_started = self.activate_owner_rt_period_for_policy(target, policy);

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
                remote.request_reschedule();
            }
            self.publish_owner_deadline_refresh_reserved(core, target, publication);
        } else {
            drop(publication);
            if preempts_current {
                remote.request_remote_reschedule();
            }
        }
        if rt_deadline_push_pending && !preempts_current {
            // Linux queues the RT/DL push balance callback in the enqueue
            // transaction. The target owner performs migration after dropping
            // the wakee's rq lock and revalidates the pushable candidate.
            remote.kick_scheduler_work();
        }
        if rt_period_started {
            remote.kick_scheduler_work();
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
        let mut sched = core.sched().lock();
        let preempts_current =
            self.enqueue_owner_thread_locked(cpu.as_mut(), &core, &mut sched, reason)?;
        let affinity_completed = Self::complete_affinity_if_satisfied_locked(&core, &sched);
        drop(sched);
        if affinity_completed {
            core.notify_affinity_waiters();
        }
        self.finish_owner_enqueue(cpu, reason, preempts_current);
        Ok(())
    }

    pub(in crate::system::task_system) fn enqueue_owner_thread_locked(
        &self,
        mut cpu: Pin<&mut CpuLocal>,
        core: &Arc<ThreadCore>,
        sched: &mut ThreadSchedState,
        reason: EnqueueReason,
    ) -> Result<bool, TaskError> {
        let owner = cpu.owner();
        if sched.lifecycle.state() != ThreadState::Ready {
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
        let mut transaction = OwnerRqTxn::begin(self, &remote);
        let now_ns = transaction.clock().wall().as_nanos();
        let policy = sched.policy.active().policy();
        let mut queued_entity = sched.policy.active().entity().clone();
        if matches!(reason, EnqueueReason::Wake)
            && matches!(policy, SchedulePolicy::Deadline(_))
            && !sched.is_pi_boosted()
        {
            queued_entity.activate_deadline(now_ns);
            *sched.policy.active_mut().entity_mut() = queued_entity.clone();
        }
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
            return Ok(preempts_current);
        }
        let preempts_current =
            self.link_owner_ready_thread_locked(owner, &mut transaction, core, sched, reason);
        let timer_preempts = self
            .refresh_owner_deadline_timers_in_rq(core, sched, cpu, now_ns, &mut transaction)
            .unwrap_or(false);
        transaction.commit();
        Ok(preempts_current || timer_preempts)
    }
}
