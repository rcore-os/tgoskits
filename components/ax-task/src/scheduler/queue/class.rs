//! Static scheduler-class chain used by one owner runqueue.
//!
//! Linux orders statically linked `sched_class` objects and enters each class
//! through the same lifecycle hooks. ax-task has a closed policy set, so an
//! enum expresses that chain without trait objects or compatibility dispatch.

use super::*;
use crate::{
    DeadlineEntity, DispatchCharge, FairMode, SchedulePolicy, SchedulingEntity,
    runtime::task_runtime,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SchedulerClass {
    Stop,
    Deadline,
    Realtime,
    /// Linux `fair_sched_class` covers Normal, Batch, and SCHED_IDLE. The
    /// per-CPU dedicated idle thread is not a member of this class: it lives
    /// in the owner's idle slot (`take_idle_schedule`) and is picked only
    /// after this class yields.
    Fair,
}

pub(super) struct ClassEnqueue {
    pub(super) membership: QueueMembershipClass,
    pub(super) entity: SchedulingEntity,
    pub(super) reason: EnqueueReason,
}

#[derive(Clone, Copy)]
pub(crate) struct ClassTick {
    pub(crate) request_reschedule: bool,
}

impl SchedulerClass {
    pub(super) const PICK_ORDER: [Self; 4] =
        [Self::Stop, Self::Deadline, Self::Realtime, Self::Fair];

    pub(crate) const fn for_policy(policy: SchedulePolicy) -> Self {
        match policy {
            SchedulePolicy::KernelStop => Self::Stop,
            SchedulePolicy::Deadline(_) => Self::Deadline,
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. } => Self::Realtime,
            // Linux maps SCHED_IDLE onto fair_sched_class; only the entity's
            // weight (WEIGHT_IDLEPRIO) and wakeup-preemption direction differ.
            SchedulePolicy::Fair { .. } => Self::Fair,
        }
    }

    /// Linux `enqueue_task()` class hook. Common rq accounting and membership
    /// publication are committed by [`RunQueue::enqueue_task`] after this
    /// hook has installed the class-owned intrusive node.
    pub(super) fn enqueue_task(
        self,
        run_queue: &mut RunQueue,
        mut thread: QueuedThread,
        reason: EnqueueReason,
        current_fair: Option<FairEntity>,
    ) -> Result<ClassEnqueue, TaskError> {
        if let SchedulingEntity::Fair(fair) = thread.active.entity_mut() {
            let virtual_time = run_queue.virtual_time();
            match reason {
                EnqueueReason::Wake => {
                    let (queue_weight, current_weight) =
                        run_queue.fair_placement_weights(current_fair);
                    fair.place_after_activation(
                        virtual_time,
                        queue_weight.saturating_add(current_weight),
                    )?;
                }
                EnqueueReason::Preempted => {}
                EnqueueReason::Yield => fair.yield_request(virtual_time),
                EnqueueReason::Migrated | EnqueueReason::PolicyChanged => {
                    let (queue_weight, current_weight) =
                        run_queue.fair_placement_weights(current_fair);
                    fair.place_after_transfer(
                        virtual_time,
                        queue_weight.saturating_add(current_weight),
                    )?;
                }
                EnqueueReason::Replenished => fair.place_at_least(virtual_time),
            }
            if !matches!(reason, EnqueueReason::Wake | EnqueueReason::Yield)
                && fair.request_exhausted()
            {
                fair.renew_request();
            }
        }
        let entity = thread.active.entity().clone();
        let membership = match self {
            Self::Stop => {
                thread.migration_capable = false;
                assert!(
                    run_queue.stop.replace(thread).is_none(),
                    "one CPU runqueue can own only one stopper task"
                );
                QueueMembershipClass::Stop
            }
            Self::Deadline => {
                if thread.active.entity().deadline().is_none_or(|deadline| {
                    deadline.absolute_deadline_ns().is_none() || deadline.is_throttled()
                }) {
                    return Err(TaskError::NotReady);
                }
                QueueMembershipClass::Deadline(run_queue.deadline.insert(thread))
            }
            Self::Realtime => QueueMembershipClass::Realtime(run_queue.rt.enqueue(thread, reason)),
            Self::Fair => {
                run_queue.fair.insert(thread);
                QueueMembershipClass::Fair
            }
        };
        Ok(ClassEnqueue {
            membership,
            entity,
            reason,
        })
    }

    /// Linux `dequeue_task()` class hook. The caller owns `nr_running`,
    /// `nr_queued`, placement demand, and public membership accounting.
    pub(super) fn dequeue_task(
        self,
        run_queue: &mut RunQueue,
        membership: QueueMembershipClass,
        id: ThreadId,
    ) -> Option<QueuedThread> {
        match (self, membership) {
            (Self::Stop, QueueMembershipClass::Stop) => run_queue.stop.take(),
            (Self::Deadline, QueueMembershipClass::Deadline(key)) => run_queue.deadline.remove(key),
            (Self::Realtime, QueueMembershipClass::Realtime(key)) => run_queue.rt.remove(key),
            (Self::Fair, QueueMembershipClass::Fair) => run_queue.fair.remove(id),
            _ => task_runtime::fatal_invariant(0x5251_1001, id.as_u64() as usize),
        }
    }

    /// Linux `migrate_task_rq()` class hook. The class removes its intrusive
    /// node and transfers policy-local placement state while the common rq
    /// layer owns runnable accounting and public membership.
    pub(super) fn migrate_task_rq(
        self,
        run_queue: &mut RunQueue,
        membership: QueueMembershipClass,
        id: ThreadId,
        timing_granularity_ns: u64,
    ) -> Option<QueuedThread> {
        if self == Self::Stop {
            return None;
        }
        // Linux `dequeue_entity()` calls `update_entity_lag()` while the
        // entity is still on cfs_rq, before `__dequeue_entity()` changes the
        // weighted average. Capture the same source-rq value before removing
        // our intrusive node; post-dequeue virtual time is not the task's
        // migration lag.
        let source_virtual_time = run_queue
            .queued_thread_including_current(id)
            .filter(|thread| thread.base_entity.fair().is_some())
            .map(|_| run_queue.virtual_time());
        let mut thread = self.dequeue_task(run_queue, membership, id)?;
        if let Some(source_virtual_time) = source_virtual_time {
            thread
                .active
                .base_entity_mut()
                .capture_fair_migration(source_virtual_time, timing_granularity_ns);
        }
        Some(thread)
    }

    /// Linux `pick_task()` class hook. RT and Deadline return a snapshot while
    /// retaining their current node in the active structure; Fair and stop
    /// transfer the selected node until `set_next_task()` commits.
    pub(super) fn pick_task(
        self,
        run_queue: &mut RunQueue,
        rt_eligibility: RtEligibility,
        skip_delayed: bool,
    ) -> Option<PickTaskResult> {
        match self {
            Self::Stop => run_queue
                .stop
                .take()
                .map(PickedThread::Owned)
                .map(PickTaskResult::Continue),
            Self::Deadline => {
                #[cfg(feature = "task-test-hooks")]
                let _snapshot_scope =
                    crate::task_test_hooks::enter_linked_pick_full_snapshot_scope();
                let picked = run_queue.deadline.select_first();
                picked
                    .map(PickedThread::Linked)
                    .map(PickTaskResult::Continue)
            }
            Self::Realtime => {
                #[cfg(feature = "task-test-hooks")]
                let _snapshot_scope =
                    crate::task_test_hooks::enter_linked_pick_full_snapshot_scope();
                let picked = matches!(rt_eligibility, RtEligibility::Runnable)
                    .then(|| run_queue.rt.select())
                    .flatten();
                picked
                    .map(PickedThread::Linked)
                    .map(PickTaskResult::Continue)
            }
            Self::Fair => {
                run_queue.update_fair_virtual_time(None);
                let queue = &mut run_queue.fair;
                let virtual_time = queue.virtual_time();
                let mut thread = match queue.pick_eligible(virtual_time, skip_delayed)? {
                    FairPick::Runnable(thread) => thread,
                    FairPick::Delayed(core) => {
                        return Some(PickTaskResult::Break(core));
                    }
                };
                let shortest_competing_slice_ns = queue.min_service_request_ns();
                let SchedulingEntity::Fair(fair) = thread.active.entity_mut() else {
                    unreachable!("FairRunQueue can select only Fair entities")
                };
                fair.set_slice_protection(shortest_competing_slice_ns);
                Some(PickTaskResult::Continue(PickedThread::Owned(thread)))
            }
        }
    }

    /// Reverses a class pick that failed owner-rq validation before set-next.
    pub(super) fn rollback_pick(self, run_queue: &mut RunQueue, mut thread: QueuedThread) {
        thread.active.entity_mut().cancel_fair_migration();
        match self {
            Self::Stop => assert!(run_queue.stop.replace(thread).is_none()),
            Self::Fair => run_queue.fair.insert(thread),
            Self::Deadline | Self::Realtime => {
                unreachable!("linked classes never transfer their node during pick")
            }
        }
    }

    /// Linux class-specific `put_prev_task()` hook for linked current classes.
    pub(super) fn put_prev_task(
        self,
        run_queue: &mut RunQueue,
        membership: QueueMembershipClass,
        id: ThreadId,
        reason: EnqueueReason,
    ) -> Result<SchedulingEntity, TaskError> {
        match (self, membership) {
            (Self::Deadline, QueueMembershipClass::Deadline(key)) => {
                let (new_key, entity) = run_queue
                    .deadline
                    .put_prev_current(key)
                    .ok_or(TaskError::NotReady)?;
                run_queue.replace_membership_class(id, QueueMembershipClass::Deadline(new_key));
                Ok(entity)
            }
            (Self::Realtime, QueueMembershipClass::Realtime(key)) => run_queue
                .rt
                .put_prev_current(key, reason)
                .ok_or(TaskError::NotReady),
            _ => Err(TaskError::InvalidConfiguration),
        }
    }

    /// Linux class-specific `set_next_task()` ownership transition.
    pub(super) fn set_next_task(self, run_queue: &mut RunQueue, picked: &PickedThread) {
        match self {
            // RT/DL keep their active-class linkage. `rq->curr`, installed by
            // the common owner transaction immediately after this hook, is
            // the sole marker that distinguishes the running node from
            // queued candidates.
            Self::Deadline | Self::Realtime => {}
            Self::Stop | Self::Fair => {
                run_queue.unregister_membership(picked.id());
            }
        }
    }

    /// Linux `task_tick()` class hook. Runtime accounting itself is common rq
    /// state; the class owns the policy-specific reschedule decision.
    pub(crate) fn task_tick(
        self,
        run_queue: &mut RunQueue,
        current: ThreadId,
        policy: SchedulePolicy,
        current_entity: &SchedulingEntity,
        charge: DispatchCharge,
    ) -> ClassTick {
        ClassTick {
            request_reschedule: match self {
                // Linux `update_curr_dl_se()` always dequeues and reschedules
                // an exhausted CBS entity. `SCHED_FLAG_DL_OVERRUN` controls
                // only user-visible overrun notification, never whether the
                // throttled task may keep running.
                Self::Deadline => charge.slice_expired,
                Self::Realtime => match policy {
                    SchedulePolicy::RoundRobin { .. } if charge.slice_expired => {
                        let key = match run_queue.membership_class(current) {
                            Some(QueueMembershipClass::Realtime(key)) => key,
                            _ => task_runtime::fatal_invariant(
                                0x5251_1010,
                                current.as_u64() as usize,
                            ),
                        };
                        run_queue
                            .rt
                            .task_tick_round_robin(key, policy)
                            .unwrap_or_else(|| {
                                task_runtime::fatal_invariant(
                                    0x5251_1010,
                                    current.as_u64() as usize,
                                )
                            })
                    }
                    SchedulePolicy::RoundRobin { .. } | SchedulePolicy::Fifo { .. } => false,
                    _ => task_runtime::fatal_invariant(0x5251_1011, current.as_u64() as usize),
                },
                // Linux v7.1 requests lazy rescheduling when either the full
                // request expires or RUN_TO_PARITY protection ends. A lone
                // current still keeps running without a Fair clockevent.
                // Linux keeps SCHED_IDLE in the same cfs_rq as Normal and
                // Batch: `nr_queued > 1` counts every fair-policy contender
                // regardless of mode.
                Self::Fair => {
                    fair_tick_requests_reschedule(run_queue.has_fair(), current_entity, charge)
                }
                Self::Stop => false,
            },
        }
    }

    pub(super) fn check_preempt_curr(
        self,
        current_policy: SchedulePolicy,
        current_entity: &SchedulingEntity,
        current_is_idle: bool,
        wakee_policy: SchedulePolicy,
        wakee_entity: &SchedulingEntity,
        fair_virtual_time: u64,
    ) -> bool {
        if current_is_idle {
            return true;
        }
        match self {
            Self::Stop => !matches!(current_policy, SchedulePolicy::KernelStop),
            Self::Deadline => match current_policy {
                SchedulePolicy::KernelStop => false,
                SchedulePolicy::Deadline(_) => {
                    deadline_key(wakee_entity) < deadline_key(current_entity)
                }
                _ => true,
            },
            Self::Realtime => {
                let wakee_priority = wakee_policy
                    .rt_priority()
                    .expect("RT wakee must carry a fixed priority");
                match current_policy {
                    SchedulePolicy::KernelStop | SchedulePolicy::Deadline(_) => false,
                    SchedulePolicy::Fifo { priority: current }
                    | SchedulePolicy::RoundRobin {
                        priority: current, ..
                    } => wakee_priority > current,
                    SchedulePolicy::Fair { .. } => true,
                }
            }
            Self::Fair => fair_wakeup_preempts(
                current_policy,
                current_entity,
                wakee_policy,
                wakee_entity,
                fair_virtual_time,
            ),
        }
    }
}

/// Linux `wakeup_preempt()` dispatch for the static class chain.
pub(crate) fn wakeup_preempts(
    current_policy: SchedulePolicy,
    current_entity: &SchedulingEntity,
    current_is_idle: bool,
    wakee_policy: SchedulePolicy,
    wakee_entity: &SchedulingEntity,
    fair_virtual_time: u64,
) -> bool {
    SchedulerClass::for_policy(wakee_policy).check_preempt_curr(
        current_policy,
        current_entity,
        current_is_idle,
        wakee_policy,
        wakee_entity,
        fair_virtual_time,
    )
}

/// Linux v7.1's default `WF_SYNC` wakeup-preemption decision.
///
/// `preempt_sync()` is nested under the disabled-by-default `NEXT_BUDDY`
/// feature. ax-task does not implement that buddy state, so a synchronous wake
/// uses the ordinary class/EEVDF decision. `WF_SYNC` still affects CPU
/// selection before the task reaches its target runqueue.
pub(crate) fn default_sync_wakeup_preempts(
    current_policy: SchedulePolicy,
    current_entity: &SchedulingEntity,
    current_is_idle: bool,
    wakee_policy: SchedulePolicy,
    wakee_entity: &SchedulingEntity,
    fair_virtual_time: u64,
) -> bool {
    wakeup_preempts(
        current_policy,
        current_entity,
        current_is_idle,
        wakee_policy,
        wakee_entity,
        fair_virtual_time,
    )
}

fn fair_wakeup_preempts(
    current_policy: SchedulePolicy,
    current_entity: &SchedulingEntity,
    wakee_policy: SchedulePolicy,
    wakee_entity: &SchedulingEntity,
    fair_virtual_time: u64,
) -> bool {
    match current_policy {
        SchedulePolicy::KernelStop
        | SchedulePolicy::Deadline(_)
        | SchedulePolicy::Fifo { .. }
        | SchedulePolicy::RoundRobin { .. } => false,
        SchedulePolicy::Fair {
            mode: current_mode, ..
        } => {
            let wakee_mode = match wakee_policy {
                SchedulePolicy::Fair { mode, .. } => mode,
                _ => unreachable!("fair scheduler class requires a fair policy"),
            };
            let wakee = wakee_entity
                .fair()
                .expect("fair policy must own a fair scheduling entity");
            let current = current_entity
                .fair()
                .expect("fair policy must own a fair scheduling entity");
            #[cfg(feature = "qperf-metrics")]
            crate::metrics::record_fair_wake_distances(
                crate::scheduler::virtual_delta(wakee.vruntime(), fair_virtual_time),
                crate::scheduler::virtual_delta(current.vruntime(), fair_virtual_time),
            );
            // Linux rejects wakeup preemption from SCHED_IDLE even when the
            // current entity is also idle. A non-idle wakee still immediately
            // preempts an idle current before the SCHED_BATCH check below.
            if wakee_mode == FairMode::Idle {
                false
            } else if current_mode == FairMode::Idle {
                true
            } else if wakee_mode == FairMode::Batch
                || wakee_entity
                    .fair()
                    .is_some_and(|fair| !fair.is_eligible(fair_virtual_time))
            {
                #[cfg(feature = "qperf-metrics")]
                crate::metrics::record_fair_wake_wakee_ineligible();
                false
            } else {
                if !current.is_eligible(fair_virtual_time) {
                    #[cfg(feature = "qperf-metrics")]
                    crate::metrics::record_fair_wake_current_ineligible();
                    true
                } else if current.slice_is_protected() && !wakee.has_shorter_slice_than(current) {
                    #[cfg(feature = "qperf-metrics")]
                    crate::metrics::record_fair_wake_current_protected();
                    false
                } else {
                    // PREEMPT_SHORT bypasses protection, but the wakee must
                    // still win the ordinary eligible EEVDF deadline pick.
                    let precedes = wakee.deadline_precedes(current);
                    #[cfg(feature = "qperf-metrics")]
                    crate::metrics::record_fair_wake_deadline(precedes);
                    precedes
                }
            }
        }
    }
}

fn fair_tick_requests_reschedule(
    has_queued_peer: bool,
    current_entity: &SchedulingEntity,
    charge: DispatchCharge,
) -> bool {
    let fair = current_entity
        .fair()
        .expect("Fair task_tick requires a Fair current entity");
    has_queued_peer && (charge.slice_expired || !fair.slice_is_protected())
}

fn deadline_key(entity: &SchedulingEntity) -> u64 {
    entity
        .deadline()
        .and_then(DeadlineEntity::absolute_deadline_ns)
        .expect("a runnable Deadline entity must own an absolute deadline")
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use super::*;
    use crate::{FairEntity, Nice};

    fn fair(vruntime: u64, virtual_deadline: u64) -> SchedulingEntity {
        SchedulingEntity::Fair(FairEntity::test_state(
            Nice::ZERO,
            FairMode::Normal,
            vruntime,
            virtual_deadline,
        ))
    }

    fn normal_fair_policy() -> SchedulePolicy {
        SchedulePolicy::fair(Nice::ZERO, FairMode::Normal)
    }

    /// Linux v7.1 `RUN_TO_PARITY` keeps an eligible current inside its
    /// protected slice even when an equal-slice wakee owns the earlier
    /// virtual deadline.
    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn equal_slice_wakee_does_not_break_current_slice_protection() {
        let current = fair(2_000, 3_000);
        let wakee = fair(1_000, 1_500);

        assert!(!wakeup_preempts(
            normal_fair_policy(),
            &current,
            false,
            normal_fair_policy(),
            &wakee,
            2_000,
        ));
    }

    /// An eligible current with the earlier deadline stays the EEVDF pick;
    /// a later-deadline wakee must not preempt it.
    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn eligible_wakee_with_later_deadline_keeps_eligible_current_running() {
        let current = fair(1_000, 1_500);
        let wakee = fair(900, 2_500);

        assert!(!wakeup_preempts(
            normal_fair_policy(),
            &current,
            false,
            normal_fair_policy(),
            &wakee,
            1_000,
        ));
    }

    /// An ineligible current can never be the EEVDF pick; an eligible wakee
    /// preempts it regardless of the deadline comparison.
    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn eligible_wakee_preempts_ineligible_current_without_deadline_order() {
        let current = fair(3_000, 3_100);
        let wakee = fair(1_000, 3_500);

        assert!(wakeup_preempts(
            normal_fair_policy(),
            &current,
            false,
            normal_fair_policy(),
            &wakee,
            2_000,
        ));
    }

    /// Linux v7.1 `PREEMPT_SHORT` bypasses RUN_TO_PARITY when the shorter
    /// wakee is itself the earlier eligible EEVDF request.
    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn shorter_slice_wakee_can_break_current_slice_protection() {
        let mut current = FairEntity::new(Nice::ZERO, FairMode::Normal, 100, 2_000);
        current.set_slice_protection(None);
        let wakee = FairEntity::new(Nice::ZERO, FairMode::Normal, 50, 1_000);

        assert!(wakeup_preempts(
            normal_fair_policy(),
            &SchedulingEntity::Fair(current),
            false,
            normal_fair_policy(),
            &SchedulingEntity::Fair(wakee),
            2_000,
        ));
    }

    /// Linux v7.1 defaults `NEXT_BUDDY` off, so `WF_SYNC` does not activate
    /// `preempt_sync()` and keeps the ordinary EEVDF preemption result.
    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn sync_wake_uses_ordinary_eevdf_without_next_buddy() {
        let mut current = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 2_000, 3_000);
        current.cancel_slice_protection();
        let wakee = fair(1_000, 1_500);
        let current = SchedulingEntity::Fair(current);

        assert!(wakeup_preempts(
            normal_fair_policy(),
            &current,
            false,
            normal_fair_policy(),
            &wakee,
            2_000,
        ));
        assert!(default_sync_wakeup_preempts(
            normal_fair_policy(),
            &current,
            false,
            normal_fair_policy(),
            &wakee,
            2_000,
        ));
    }
}
