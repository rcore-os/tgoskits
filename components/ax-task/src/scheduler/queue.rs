//! Run queue mutated exclusively by its owner CPU.

use alloc::{collections::VecDeque, vec::Vec};

use crate::{FairMode, SchedulePolicy, SchedulingEntity, TaskError, ThreadId};

/// Why a runnable thread is being inserted into its owner run queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueReason {
    /// Newly ready or awakened work joins the class tail.
    Wake,
    /// An explicit yield joins the class tail.
    Yield,
    /// Higher-class preemption preserves FIFO/RR position.
    Preempted,
    /// A replenished reservation becomes eligible again.
    Replenished,
    /// Runnable state was handed off by another owner CPU without a new wake.
    Migrated,
    /// The owner CPU applied a newer scheduling-policy generation.
    PolicyChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QueuedThread {
    pub(crate) id: ThreadId,
    pub(crate) policy: SchedulePolicy,
    pub(crate) entity: SchedulingEntity,
    sequence: u64,
}

#[derive(Debug)]
pub(crate) struct RunQueue {
    deadline: Vec<QueuedThread>,
    rt: [VecDeque<QueuedThread>; 99],
    fair: Vec<QueuedThread>,
    idle_fair: Vec<QueuedThread>,
    virtual_time: u64,
    earliest_deadline_event_ns: Option<u64>,
    next_sequence: u64,
    len: usize,
}

impl RunQueue {
    pub(crate) fn new() -> Self {
        Self {
            deadline: Vec::new(),
            rt: core::array::from_fn(|_| VecDeque::new()),
            fair: Vec::new(),
            idle_fair: Vec::new(),
            virtual_time: 0,
            earliest_deadline_event_ns: None,
            next_sequence: 0,
            len: 0,
        }
    }

    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    pub(crate) const fn virtual_time(&self) -> u64 {
        self.virtual_time
    }

    pub(crate) fn has_rt(&self) -> bool {
        self.rt.iter().any(|queue| !queue.is_empty())
    }

    pub(crate) fn highest_rt_priority(&self) -> Option<u8> {
        self.rt
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, queue)| (!queue.is_empty()).then_some(index as u8 + 1))
    }

    pub(crate) fn rt_count_at_priority(&self, priority: u8) -> usize {
        priority
            .checked_sub(1)
            .and_then(|index| self.rt.get(index as usize))
            .map_or(0, VecDeque::len)
    }

    pub(crate) fn has_fair(&self) -> bool {
        !self.fair.is_empty()
    }

    pub(crate) const fn earliest_deadline_event_ns(&self) -> Option<u64> {
        self.earliest_deadline_event_ns
    }

    pub(crate) fn update_deadline_entity(
        &mut self,
        id: ThreadId,
        entity: SchedulingEntity,
    ) -> bool {
        let Some(thread) = self.deadline.iter_mut().find(|thread| thread.id == id) else {
            return false;
        };
        thread.entity = entity;
        self.recompute_earliest_deadline_event();
        true
    }

    pub(crate) fn balance_candidate(
        &self,
        mut may_migrate: impl FnMut(QueuedThread) -> bool,
    ) -> Option<QueuedThread> {
        self.deadline
            .iter()
            .copied()
            .filter(|thread| may_migrate(*thread))
            .min_by_key(|thread| {
                let absolute = thread
                    .entity
                    .deadline()
                    .map_or(u64::MAX, |deadline| deadline.absolute_deadline_ns());
                (absolute, thread.sequence)
            })
            .or_else(|| {
                self.rt
                    .iter()
                    .rev()
                    .find_map(|queue| queue.iter().copied().find(|thread| may_migrate(*thread)))
            })
            .or_else(|| {
                self.fair
                    .iter()
                    .copied()
                    .filter(|thread| may_migrate(*thread))
                    .min_by_key(|thread| {
                        thread
                            .entity
                            .fair()
                            .map_or(u64::MAX, |fair| fair.virtual_deadline())
                    })
            })
    }

    pub(crate) fn enqueue(
        &mut self,
        id: ThreadId,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<(), TaskError> {
        if self.contains(id) {
            return Err(TaskError::AlreadyQueued);
        }
        let sequence = self.allocate_sequence();
        let mut entry = QueuedThread {
            id,
            policy,
            entity,
            sequence,
        };
        if let SchedulingEntity::Fair(fair) = &mut entry.entity {
            fair.place_at_least(self.virtual_time);
            if matches!(reason, EnqueueReason::Yield) || fair.request_exhausted() {
                fair.renew_request(self.virtual_time);
            }
        }
        let reason = if matches!(reason, EnqueueReason::Yield)
            || (matches!(reason, EnqueueReason::Preempted)
                && entry.entity.round_robin_quantum_expired())
        {
            entry.entity.reset_round_robin_quantum(policy);
            EnqueueReason::Yield
        } else {
            reason
        };
        match policy {
            SchedulePolicy::Deadline(_) => {
                if reason == EnqueueReason::Wake {
                    entry.entity.activate_deadline(now_ns);
                }
                if entry.entity.deadline().is_none_or(|deadline| {
                    deadline.absolute_deadline_ns() == 0 || deadline.is_throttled()
                }) {
                    return Err(TaskError::NotReady);
                }
                self.deadline.push(entry);
                self.recompute_earliest_deadline_event();
            }
            SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
                let queue = &mut self.rt[(priority.get() - 1) as usize];
                if reason == EnqueueReason::Preempted {
                    queue.push_front(entry);
                } else {
                    queue.push_back(entry);
                }
            }
            SchedulePolicy::Fair {
                mode: FairMode::Idle,
                ..
            } => self.idle_fair.push(entry),
            SchedulePolicy::Fair { .. } => self.fair.push(entry),
        }
        self.len += 1;
        Ok(())
    }

    pub(crate) fn dequeue(&mut self, id: ThreadId) -> Option<QueuedThread> {
        let removed = remove_from_vec(&mut self.deadline, id)
            .or_else(|| remove_from_rt(&mut self.rt, id))
            .or_else(|| remove_from_vec(&mut self.fair, id))
            .or_else(|| remove_from_vec(&mut self.idle_fair, id));
        if removed.is_some() {
            self.len -= 1;
            self.recompute_earliest_deadline_event();
        }
        removed
    }

    pub(crate) fn pick_next_with_rt(
        &mut self,
        ordinary_rt_may_run: bool,
        mut is_pi_boosted_owner: impl FnMut(ThreadId) -> bool,
    ) -> Option<QueuedThread> {
        let picked = self
            .pick_deadline()
            .or_else(|| self.pick_rt(ordinary_rt_may_run, &mut is_pi_boosted_owner))
            .or_else(|| self.pick_fair(false))
            .or_else(|| self.pick_fair(true));
        if picked.is_some() {
            self.len -= 1;
        }
        picked
    }

    fn pick_deadline(&mut self) -> Option<QueuedThread> {
        let index = self
            .deadline
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| {
                let absolute = match entry.entity {
                    SchedulingEntity::Deadline(entity) => entity.absolute_deadline_ns(),
                    _ => u64::MAX,
                };
                (absolute, entry.sequence)
            })
            .map(|(index, _)| index)?;
        let picked = self.deadline.swap_remove(index);
        self.recompute_earliest_deadline_event();
        Some(picked)
    }

    fn pick_rt(
        &mut self,
        ordinary_rt_may_run: bool,
        is_pi_boosted_owner: &mut impl FnMut(ThreadId) -> bool,
    ) -> Option<QueuedThread> {
        for queue in self.rt.iter_mut().rev() {
            if ordinary_rt_may_run {
                if let Some(thread) = queue.pop_front() {
                    return Some(thread);
                }
            } else if let Some(index) = queue
                .iter()
                .position(|thread| is_pi_boosted_owner(thread.id))
            {
                return queue.remove(index);
            }
        }
        None
    }

    fn pick_fair(&mut self, idle: bool) -> Option<QueuedThread> {
        let queue = if idle {
            &mut self.idle_fair
        } else {
            &mut self.fair
        };
        if queue.is_empty() {
            return None;
        }
        let minimum_vruntime = queue
            .iter()
            .filter_map(|entry| entry.entity.fair().map(|entity| entity.vruntime()))
            .min()
            .unwrap_or(self.virtual_time);
        self.virtual_time = self.virtual_time.max(minimum_vruntime);
        let index = queue
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry
                    .entity
                    .fair()
                    .is_some_and(|entity| entity.is_eligible(self.virtual_time))
            })
            .min_by_key(|(_, entry)| {
                let deadline = entry
                    .entity
                    .fair()
                    .map(|entity| entity.virtual_deadline())
                    .unwrap_or(u64::MAX);
                (deadline, entry.sequence)
            })
            .map(|(index, _)| index)?;
        Some(queue.swap_remove(index))
    }

    fn recompute_earliest_deadline_event(&mut self) {
        self.earliest_deadline_event_ns = self
            .deadline
            .iter()
            .filter_map(|thread| thread.entity.deadline())
            .map(|deadline| deadline.next_scheduler_event_ns())
            .filter(|deadline| *deadline != 0)
            .min();
    }

    fn contains(&self, id: ThreadId) -> bool {
        self.deadline.iter().any(|entry| entry.id == id)
            || self
                .rt
                .iter()
                .any(|queue| queue.iter().any(|entry| entry.id == id))
            || self.fair.iter().any(|entry| entry.id == id)
            || self.idle_fair.iter().any(|entry| entry.id == id)
    }

    fn allocate_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        sequence
    }
}

fn remove_from_vec(queue: &mut Vec<QueuedThread>, id: ThreadId) -> Option<QueuedThread> {
    let index = queue.iter().position(|entry| entry.id == id)?;
    Some(queue.swap_remove(index))
}

fn remove_from_rt(queues: &mut [VecDeque<QueuedThread>; 99], id: ThreadId) -> Option<QueuedThread> {
    for queue in queues {
        if let Some(index) = queue.iter().position(|entry| entry.id == id) {
            return queue.remove(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority};

    #[test]
    fn deadline_precedes_rt_and_fair() {
        let mut queue = RunQueue::new();
        let fair = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let rt = SchedulePolicy::fifo(RtPriority::new(99).unwrap());
        let deadline =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
        queue
            .enqueue(
                ThreadId::from_parts(0, 1),
                fair,
                SchedulingEntity::new(fair, 1, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
        queue
            .enqueue(
                ThreadId::from_parts(1, 1),
                rt,
                SchedulingEntity::new(rt, 1, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
        queue
            .enqueue(
                ThreadId::from_parts(2, 1),
                deadline,
                SchedulingEntity::new(deadline, 1, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
        assert_eq!(
            queue.pick_next_with_rt(true, |_| false).unwrap().id,
            ThreadId::from_parts(2, 1)
        );
    }

    #[test]
    fn fifo_preemption_preserves_the_head_position() {
        let mut queue = RunQueue::new();
        let policy = SchedulePolicy::fifo(RtPriority::new(10).unwrap());
        for slot in [1, 2] {
            queue
                .enqueue(
                    ThreadId::from_parts(slot, 1),
                    policy,
                    SchedulingEntity::new(policy, 1, 0),
                    0,
                    EnqueueReason::Wake,
                )
                .unwrap();
        }
        queue
            .enqueue(
                ThreadId::from_parts(0, 1),
                policy,
                SchedulingEntity::new(policy, 1, 0),
                0,
                EnqueueReason::Preempted,
            )
            .unwrap();
        assert_eq!(
            queue.pick_next_with_rt(true, |_| false).unwrap().id,
            ThreadId::from_parts(0, 1)
        );
    }

    #[test]
    fn first_fair_placement_cannot_start_behind_runqueue_virtual_time() {
        let mut queue = RunQueue::new();
        queue.virtual_time = 10_000;
        let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let thread = ThreadId::from_parts(0, 1);

        queue
            .enqueue(
                thread,
                policy,
                SchedulingEntity::new(policy, 1_000, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();

        let entity = queue.dequeue(thread).unwrap().entity.fair().unwrap();
        assert_eq!(entity.vruntime(), 10_000);
        assert_eq!(entity.virtual_deadline(), 11_000);
    }

    #[test]
    fn deadline_preemption_does_not_reapply_the_cbs_wake_rule() {
        let mut queue = RunQueue::new();
        let policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(4, 8, 10, DeadlineFlags::NONE).unwrap());
        let thread = ThreadId::from_parts(0, 1);
        let mut entity = SchedulingEntity::new(policy, 1, 0);
        entity.activate_deadline(0);
        assert!(!entity.charge(1, 0, 0));

        queue
            .enqueue(thread, policy, entity, 4, EnqueueReason::Preempted)
            .unwrap();

        let deadline = queue.dequeue(thread).unwrap().entity.deadline().unwrap();
        assert_eq!(deadline.absolute_deadline_ns(), 8);
        assert_eq!(deadline.remaining_runtime_ns(), 3);
    }
}
