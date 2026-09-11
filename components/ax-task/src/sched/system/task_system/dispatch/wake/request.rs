//! Request under the owning scheduler transaction.

use super::*;

enum WakeSource {
    Ordinary,
    RtLock { generation: u64 },
    RtLockPark { generation: u64 },
}

impl TaskSystem {
    /// Wakes a blocked thread from the runtime's current CPU.
    pub(crate) fn wake_thread_from_current_cpu(
        &self,
        core: &Arc<ThreadCore>,
        intent: WakeIntent,
    ) -> WakeResult {
        self.wake_thread(core, intent)
    }

    pub(super) fn wake_thread(&self, core: &Arc<ThreadCore>, intent: WakeIntent) -> WakeResult {
        self.wake_thread_source(core, intent, WakeSource::Ordinary)
    }

    pub(in crate::sched::system) fn wake_rt_lock_thread(
        &self,
        core: &Arc<ThreadCore>,
        generation: u64,
    ) -> WakeResult {
        self.wake_thread_source(core, WakeIntent::Normal, WakeSource::RtLock { generation })
    }

    pub(crate) fn wake_rt_lock_park(&self, core: &Arc<ThreadCore>, generation: u64) -> WakeResult {
        self.wake_thread_source(
            core,
            WakeIntent::Normal,
            WakeSource::RtLockPark { generation },
        )
    }

    fn wake_thread_source(
        &self,
        core: &Arc<ThreadCore>,
        intent: WakeIntent,
        source: WakeSource,
    ) -> WakeResult {
        #[cfg(feature = "qperf-metrics")]
        crate::diagnostics::counters::record_direct_wake_attempt();
        // A direct wake owns an Arc-backed task handle, so its lifetime is
        // already independent of the reaper. Linux serializes this producer
        // with exit through `p->pi_lock`; the task scheduler lock is the same
        // ownership boundary here. The preempt scope only pins the producer
        // while selecting its CPU and acquiring that lock.
        // Linux enters the same preemption guard for every try_to_wake_up()
        // caller. The runtime guard itself inherits an existing hardirq or
        // scheduler baton, so the wake path must not probe IRQ context first.
        let _preempt = crate::runtime::lock::PreemptScope::enter();
        let context = WakeTransactionContext::current();
        let sched = core.sched().lock();
        let wake_publication = match source {
            WakeSource::Ordinary => core.publish_wake(),
            WakeSource::RtLockPark { generation } => {
                if core.park_generation() != generation {
                    return WakeResult::Notified;
                }
                let Some(publication) = core.publish_rt_lock_wake() else {
                    return WakeResult::Notified;
                };
                publication
            }
            WakeSource::RtLock { generation } => {
                if sched
                    .pi
                    .blocked_on
                    .is_none_or(|wait| wait.generation != generation)
                {
                    return WakeResult::Notified;
                }
                let Some(publication) = core.publish_rt_lock_wake() else {
                    return WakeResult::Notified;
                };
                publication
            }
        };

        if wake_publication.saved_state_only() {
            return WakeResult::Notified;
        }
        if wake_publication.already_pending() && wake_publication.state() != ThreadState::Blocked {
            return WakeResult::AlreadyPending;
        }
        match wake_publication.state() {
            ThreadState::Parking
            | ThreadState::Running
            | ThreadState::Waking
            | ThreadState::New => return WakeResult::Notified,
            ThreadState::Exited => {
                core.discard_failed_wake();
                return WakeResult::Exited;
            }
            ThreadState::Blocked => {}
        }

        if sched.lifecycle.state() == ThreadState::Exited {
            core.discard_failed_wake();
            return WakeResult::Exited;
        }
        if matches!(
            sched.lifecycle.state(),
            ThreadState::Parking | ThreadState::Running | ThreadState::Waking
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
            let transition = Self::consume_on_rq_wake_locked(core);
            if transition != WakeTransition::Activate {
                task_runtime::fatal_invariant(0x574b_000f, core.id().as_u64() as usize);
            }

            return self.wake_on_rq_locked(core, sched, target, intent, context);
        }
        let previous = sched
            .placement
            .assigned_cpu()
            .or_else(|| core.wake_cpu_hint());
        let target =
            self.select_wake_target(&sched, core, Some(context.producer), previous, intent);

        let Some(target) = target else {
            return WakeResult::Unavailable;
        };
        let transition = Self::consume_wake_locked(core);
        match transition {
            WakeTransition::Notified => WakeResult::Notified,
            WakeTransition::Activate => {
                self.activate_waking_thread_locked(core, sched, target, intent, context)
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
        core: &Arc<ThreadCore>,
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
        let _preempt = crate::runtime::lock::PreemptScope::enter();
        let context = WakeTransactionContext::current();
        let sched = core.sched().lock();
        if core.ordinary_park_generation() != claim.park_generation() {
            claim.cancel_selected();
            return WaitWakeDelivery::Cancelled;
        }
        if core.in_rt_lock_wait() {
            if !claim.deliver_selected() {
                return WaitWakeDelivery::Cancelled;
            }
            let publication = core.publish_wake();
            debug_assert!(publication.saved_state_only());
            return WaitWakeDelivery::Delivered;
        }
        match sched.lifecycle.state() {
            ThreadState::Parking => {
                if !claim.deliver_selected() {
                    return WaitWakeDelivery::Cancelled;
                }

                // The sticky publication normally makes the owner's final
                // park CAS restore Running. The rq-only block path may win
                // that CAS immediately before this store; the state returned
                // by fetch_or then proves that this waker must finish wakeup
                // instead of leaving a sleeping task with a pending bit.
                let wake = core.publish_wake();
                if wake.state() != ThreadState::Blocked {
                    return WaitWakeDelivery::Delivered;
                }
                // The rq-only parker does not take this task lock. It reserves
                // ownership publication before publishing Blocked, so a claim
                // which acquired the task lock while the state was Parking can
                // observe Blocked before detached or delayed rq ownership is
                // visible. Drop that stale guard and reacquire through the
                // publication-aware path, matching Linux's on_rq revalidation
                // under p->pi_lock.
                drop(sched);
                let sched = core.sched().lock();
                if core.park_generation() != claim.park_generation()
                    || sched.lifecycle.state() != ThreadState::Blocked
                {
                    return WaitWakeDelivery::Delivered;
                }
                let assigned = sched.placement.assigned_cpu().unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x574b_0017, core.id().as_u64() as usize)
                });
                let target = sched.placement.queued_cpu().unwrap_or(assigned);
                let on_rq = sched.placement.queued_cpu() == Some(target);
                let transition = if on_rq {
                    Self::consume_on_rq_wake_locked(core)
                } else {
                    Self::consume_wake_locked(core)
                };
                if transition != WakeTransition::Activate {
                    task_runtime::fatal_invariant(0x574b_001b, core.id().as_u64() as usize);
                }
                let result = if on_rq {
                    self.wake_on_rq_locked(core, sched, target, intent, context)
                } else {
                    self.activate_waking_thread_locked(core, sched, target, intent, context)
                };
                if result != WakeResult::Notified {
                    task_runtime::fatal_invariant(0x574b_001c, core.id().as_u64() as usize);
                }
                WaitWakeDelivery::Delivered
            }
            ThreadState::Blocked => {
                if let Some(target) = sched.placement.queued_cpu() {
                    if !claim.deliver_selected() {
                        return WaitWakeDelivery::Cancelled;
                    }
                    let _already_pending = core.publish_wake();
                    let transition = Self::consume_on_rq_wake_locked(core);
                    if transition != WakeTransition::Activate {
                        task_runtime::fatal_invariant(0x574b_0011, core.id().as_u64() as usize);
                    }
                    let result = self.wake_on_rq_locked(core, sched, target, intent, context);
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
                    let Some(target) = self.select_wake_target(
                        &sched,
                        core,
                        Some(context.producer),
                        previous,
                        intent,
                    ) else {
                        claim.cancel_selected();
                        return WaitWakeDelivery::Unavailable;
                    };
                    target
                };

                if !claim.deliver_selected() {
                    return WaitWakeDelivery::Cancelled;
                }
                let _already_pending = core.publish_wake();
                let transition = Self::consume_wake_locked(core);
                if transition != WakeTransition::Activate {
                    task_runtime::fatal_invariant(0x574b_000c, core.id().as_u64() as usize);
                }
                let result =
                    self.activate_waking_thread_locked(core, sched, target, intent, context);
                if result != WakeResult::Notified {
                    task_runtime::fatal_invariant(0x574b_000d, core.id().as_u64() as usize);
                }
                WaitWakeDelivery::Delivered
            }
            ThreadState::Exited => {
                claim.cancel_selected();
                WaitWakeDelivery::Exited
            }
            ThreadState::New | ThreadState::Running | ThreadState::Waking => {
                // Another wake source or a later park generation owns the
                // runnable state. Do not leave a notification for its next
                // park attempt.
                claim.cancel_selected();
                WaitWakeDelivery::Cancelled
            }
        }
    }
}
