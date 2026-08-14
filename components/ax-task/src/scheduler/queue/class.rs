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
    Fair,
    IdleFair,
}

pub(super) struct ClassEnqueue {
    pub(super) membership: QueueMembershipClass,
    pub(super) entity: SchedulingEntity,
    pub(super) reason: EnqueueReason,
}

#[derive(Clone, Copy)]
pub(crate) struct ClassTick {
    pub(crate) request_reschedule: bool,
    pub(crate) realtime: bool,
}

impl SchedulerClass {
    pub(super) const PICK_ORDER: [Self; 5] = [
        Self::Stop,
        Self::Deadline,
        Self::Realtime,
        Self::Fair,
        Self::IdleFair,
    ];

    pub(crate) const fn for_policy(policy: SchedulePolicy) -> Self {
        match policy {
            SchedulePolicy::KernelStop => Self::Stop,
            SchedulePolicy::Deadline(_) => Self::Deadline,
            SchedulePolicy::Fifo { .. } | SchedulePolicy::RoundRobin { .. } => Self::Realtime,
            SchedulePolicy::Fair {
                mode: FairMode::Idle,
                ..
            } => Self::IdleFair,
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
            let virtual_time = run_queue.virtual_time_for_mode(fair.mode());
            match reason {
                EnqueueReason::Wake => {
                    let (queue_weight, current_weight) =
                        run_queue.fair_placement_weights(*fair, current_fair);
                    fair.place_after_activation(
                        virtual_time,
                        queue_weight.saturating_add(current_weight),
                    )?;
                }
                EnqueueReason::Preempted => {}
                EnqueueReason::Yield => fair.yield_request(virtual_time),
                EnqueueReason::Migrated | EnqueueReason::PolicyChanged => {
                    let (queue_weight, current_weight) =
                        run_queue.fair_placement_weights(*fair, current_fair);
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
                fair.renew_request(virtual_time);
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
            Self::Realtime => {
                let priority = thread
                    .active
                    .policy()
                    .rt_priority()
                    .expect("RT class policy must carry a fixed priority");
                let queued_priority = run_queue.rt.enqueue(thread, reason);
                debug_assert_eq!(queued_priority, priority.get());
                QueueMembershipClass::Realtime(priority.get())
            }
            Self::Fair => {
                run_queue.fair.insert(thread);
                QueueMembershipClass::Fair
            }
            Self::IdleFair => {
                run_queue.idle_fair.insert(thread);
                QueueMembershipClass::IdleFair
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
            (Self::Realtime, QueueMembershipClass::Realtime(priority)) => {
                run_queue.rt.remove(priority, id)
            }
            (Self::Fair, QueueMembershipClass::Fair) => run_queue.fair.remove(id),
            (Self::IdleFair, QueueMembershipClass::IdleFair) => run_queue.idle_fair.remove(id),
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
            .and_then(|thread| thread.base_entity.fair())
            .map(|fair| run_queue.virtual_time_for_mode(fair.mode()));
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
    ) -> Option<PickedThread> {
        match self {
            Self::Stop => run_queue.stop.take().map(PickedThread::Owned),
            Self::Deadline => run_queue.deadline.select_first().map(PickedThread::Linked),
            Self::Realtime => matches!(rt_eligibility, RtEligibility::Runnable)
                .then(|| run_queue.rt.select())
                .flatten()
                .map(PickedThread::Linked),
            Self::Fair | Self::IdleFair => {
                run_queue.update_fair_virtual_time(None);
                let queue = if self == Self::IdleFair {
                    &mut run_queue.idle_fair
                } else {
                    &mut run_queue.fair
                };
                let virtual_time = queue.virtual_time();
                (!queue.is_empty())
                    .then(|| queue.pick_eligible(virtual_time))
                    .flatten()
                    .map(PickedThread::Owned)
            }
        }
    }

    /// Reverses a class pick that failed owner-rq validation before set-next.
    pub(super) fn rollback_pick(self, run_queue: &mut RunQueue, mut thread: QueuedThread) {
        thread.active.entity_mut().cancel_fair_migration();
        match self {
            Self::Stop => assert!(run_queue.stop.replace(thread).is_none()),
            Self::Fair => run_queue.fair.insert(thread),
            Self::IdleFair => run_queue.idle_fair.insert(thread),
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
            (Self::Realtime, QueueMembershipClass::Realtime(priority)) => run_queue
                .rt
                .put_prev_current(priority, id, reason)
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
            Self::Stop | Self::Fair | Self::IdleFair => {
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
                    SchedulePolicy::RoundRobin { priority, .. } if charge.slice_expired => {
                        run_queue
                            .rt
                            .task_tick_round_robin(priority.get(), current, policy)
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
                Self::Fair | Self::IdleFair => charge.slice_expired,
                Self::Stop => false,
            },
            realtime: matches!(self, Self::Realtime),
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
            Self::Fair | Self::IdleFair => fair_wakeup_preempts(
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
            if wakee_mode == FairMode::Idle && current_mode != FairMode::Idle {
                false
            } else if wakee_mode != FairMode::Idle && current_mode == FairMode::Idle {
                true
            } else if wakee_mode == FairMode::Batch
                || wakee_entity
                    .fair()
                    .is_none_or(|fair| !fair.is_eligible(fair_virtual_time))
            {
                false
            } else {
                let wakee = wakee_entity
                    .fair()
                    .expect("fair policy must own a fair scheduling entity");
                let current = current_entity
                    .fair()
                    .expect("fair policy must own a fair scheduling entity");
                (!current.is_eligible(fair_virtual_time) || current.request_exhausted())
                    && wakee.deadline_precedes(current)
            }
        }
    }
}

fn deadline_key(entity: &SchedulingEntity) -> u64 {
    entity
        .deadline()
        .and_then(DeadlineEntity::absolute_deadline_ns)
        .expect("a runnable Deadline entity must own an absolute deadline")
}
