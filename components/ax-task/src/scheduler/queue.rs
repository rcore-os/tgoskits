//! Run queue mutated exclusively by its owner CPU.

use alloc::{collections::VecDeque, sync::Arc, vec::Vec};

use super::fair_queue::FairRunQueue;
use crate::{
    FairEntity, FairMode, SchedulePolicy, SchedulingEntity, SchedulingKey, TaskError, ThreadCore,
    ThreadId,
};

#[cfg(test)]
std::thread_local! {
    static FAIR_RUNQUEUE_VISITS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
    static RUNQUEUE_MEMBERSHIP_LOOKUPS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
}

#[cfg(test)]
fn reset_fair_runqueue_visits() {
    FAIR_RUNQUEUE_VISITS.set(0);
}

#[cfg(test)]
fn fair_runqueue_visits() -> usize {
    FAIR_RUNQUEUE_VISITS.get()
}

#[cfg(test)]
pub(super) fn record_fair_runqueue_visit() {
    FAIR_RUNQUEUE_VISITS.set(FAIR_RUNQUEUE_VISITS.get().saturating_add(1));
}

#[cfg(test)]
fn reset_runqueue_membership_lookups() {
    RUNQUEUE_MEMBERSHIP_LOOKUPS.set(0);
}

#[cfg(test)]
fn runqueue_membership_lookups() -> usize {
    RUNQUEUE_MEMBERSHIP_LOOKUPS.get()
}

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

#[derive(Clone, Debug)]
pub(crate) struct QueuedThread {
    pub(crate) id: ThreadId,
    pub(crate) policy: SchedulePolicy,
    pub(crate) entity: SchedulingEntity,
    pub(crate) core: Arc<ThreadCore>,
    pub(super) sequence: u64,
}

impl QueuedThread {
    pub(crate) fn balance_key(&self) -> SchedulingKey {
        self.entity.fair().map_or_else(
            || self.entity.scheduling_key(self.policy, self.id.as_u64()),
            |fair| {
                SchedulingKey::new(
                    self.policy.class_rank(),
                    fair.virtual_deadline(),
                    self.id.as_u64(),
                )
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PushableSummary {
    thread: ThreadId,
    key: SchedulingKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueMembershipClass {
    Deadline,
    Realtime(u8),
    Fair,
    IdleFair,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct QueueMembership {
    generation: u32,
    class: QueueMembershipClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueRestorePoint {
    Deadline(usize),
    Realtime { priority: u8, position: usize },
    Fair,
    IdleFair,
}

/// A runqueue entry detached for an owner-controlled transfer.
///
/// The restore point retains the exact FIFO position so a failed migration can
/// put the entry back without changing scheduling order.
#[derive(Debug)]
#[must_use = "a detached runqueue entry must be published or restored"]
pub(crate) struct DetachedQueueEntry {
    pub(crate) thread: QueuedThread,
    restore_point: QueueRestorePoint,
}

#[derive(Debug)]
pub(crate) struct RunQueue {
    deadline: Vec<QueuedThread>,
    rt: [VecDeque<QueuedThread>; 99],
    rt_bitmap: u128,
    fair: FairRunQueue,
    idle_fair: FairRunQueue,
    membership: Vec<Option<QueueMembership>>,
    virtual_time: u64,
    idle_virtual_time: u64,
    earliest_deadline_event_ns: Option<u64>,
    pushable_summary: Option<PushableSummary>,
    next_sequence: u64,
    len: usize,
}

impl RunQueue {
    pub(crate) fn new() -> Self {
        Self {
            deadline: Vec::new(),
            rt: core::array::from_fn(|_| VecDeque::new()),
            rt_bitmap: 0,
            fair: FairRunQueue::new(),
            idle_fair: FairRunQueue::new(),
            membership: Vec::new(),
            virtual_time: 0,
            idle_virtual_time: 0,
            earliest_deadline_event_ns: None,
            pushable_summary: None,
            next_sequence: 0,
            len: 0,
        }
    }

    pub(crate) const fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    pub(crate) const fn virtual_time(&self) -> u64 {
        self.virtual_time
    }

    pub(crate) const fn virtual_time_for_mode(&self, mode: FairMode) -> u64 {
        if matches!(mode, FairMode::Idle) {
            self.idle_virtual_time
        } else {
            self.virtual_time
        }
    }

    /// Advances each fair class's lag origin from its runnable weighted mean.
    ///
    /// `current` is supplied because the running entity is temporarily absent
    /// from the owner runqueue. Virtual time is monotonic; dequeueing a sleeper
    /// cannot move it backward and manufacture positive lag.
    pub(crate) fn update_fair_virtual_time(&mut self, current: Option<FairEntity>) {
        let normal_current = current.filter(|entity| entity.mode() != FairMode::Idle);
        let idle_current = current.filter(|entity| entity.mode() == FairMode::Idle);
        if let Some(mean) = self.fair.weighted_virtual_time(normal_current) {
            self.virtual_time = self.virtual_time.max(mean);
        }
        if let Some(mean) = self.idle_fair.weighted_virtual_time(idle_current) {
            self.idle_virtual_time = self.idle_virtual_time.max(mean);
        }
    }

    pub(crate) fn has_rt(&self) -> bool {
        self.rt_bitmap != 0
    }

    pub(crate) fn highest_rt_priority(&self) -> Option<u8> {
        (self.rt_bitmap != 0).then(|| (u128::BITS - self.rt_bitmap.leading_zeros()) as u8)
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

    pub(crate) fn has_idle_fair(&self) -> bool {
        !self.idle_fair.is_empty()
    }

    pub(crate) const fn earliest_deadline_event_ns(&self) -> Option<u64> {
        self.earliest_deadline_event_ns
    }

    pub(crate) const fn pushable_key(&self) -> Option<SchedulingKey> {
        match self.pushable_summary {
            Some(summary) => Some(summary.key),
            None => None,
        }
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
        self.recompute_pushable_summary();
        true
    }

    pub(crate) fn balance_candidate(
        &self,
        mut may_migrate: impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThread> {
        self.deadline
            .iter()
            .filter(|thread| may_migrate(thread))
            .min_by_key(|thread| {
                let absolute = thread
                    .entity
                    .deadline()
                    .map_or(u64::MAX, |deadline| deadline.absolute_deadline_ns());
                (absolute, thread.sequence)
            })
            .cloned()
            .or_else(|| {
                self.rt
                    .iter()
                    .rev()
                    .find_map(|queue| queue.iter().find(|thread| may_migrate(thread)).cloned())
            })
            .or_else(|| self.fair.find_first_matching(&mut may_migrate))
    }

    pub(crate) fn enqueue(
        &mut self,
        id: ThreadId,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        core: Arc<ThreadCore>,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<SchedulingEntity, TaskError> {
        if self.contains(id) {
            return Err(TaskError::AlreadyQueued);
        }
        let sequence = self.allocate_sequence();
        let mut entry = QueuedThread {
            id,
            policy,
            entity,
            core,
            sequence,
        };
        if let SchedulingEntity::Fair(fair) = &mut entry.entity {
            let virtual_time = self.virtual_time_for_mode(fair.mode());
            fair.place_at_least(virtual_time);
            if matches!(reason, EnqueueReason::Yield) {
                fair.yield_request(virtual_time);
            } else if fair.request_exhausted() {
                fair.renew_request(virtual_time);
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
        let queued_entity = entry.entity;
        let (pushable_summary, membership_class) = match policy {
            SchedulePolicy::Deadline(_) => {
                if reason == EnqueueReason::Wake {
                    entry.entity.activate_deadline(now_ns);
                }
                if entry.entity.deadline().is_none_or(|deadline| {
                    deadline.absolute_deadline_ns() == 0 || deadline.is_throttled()
                }) {
                    return Err(TaskError::NotReady);
                }
                let summary = Self::pushable_summary_for(&entry);
                self.deadline.push(entry);
                self.recompute_earliest_deadline_event();
                (summary, QueueMembershipClass::Deadline)
            }
            SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
                let summary = Self::pushable_summary_for(&entry);
                let queue = &mut self.rt[(priority.get() - 1) as usize];
                if reason == EnqueueReason::Preempted {
                    queue.push_front(entry);
                } else {
                    queue.push_back(entry);
                }
                self.rt_bitmap |= 1_u128 << (priority.get() - 1);
                (summary, QueueMembershipClass::Realtime(priority.get()))
            }
            SchedulePolicy::Fair {
                mode: FairMode::Idle,
                ..
            } => {
                self.idle_fair.insert(entry);
                (None, QueueMembershipClass::IdleFair)
            }
            SchedulePolicy::Fair { .. } => {
                let summary = Self::pushable_summary_for(&entry);
                self.fair.insert(entry);
                (summary, QueueMembershipClass::Fair)
            }
        };
        self.len += 1;
        self.register_membership(id, membership_class);
        self.consider_pushable_summary(pushable_summary);
        Ok(queued_entity)
    }

    #[cfg(test)]
    fn enqueue_test(
        &mut self,
        id: ThreadId,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
        now_ns: u64,
        reason: EnqueueReason,
    ) -> Result<SchedulingEntity, TaskError> {
        let sched = Arc::new(crate::ThreadSchedCell::new_test(id, policy));
        let core = Arc::new(ThreadCore::new(id, policy, sched, None, None, None));
        self.enqueue(id, policy, entity, core, now_ns, reason)
    }

    pub(crate) fn dequeue(&mut self, id: ThreadId) -> Option<QueuedThread> {
        let class = self.membership_class(id)?;
        let removed = match class {
            QueueMembershipClass::Deadline => remove_from_vec(&mut self.deadline, id),
            QueueMembershipClass::Realtime(priority) => {
                let index = (priority - 1) as usize;
                let removed = remove_from_rt_queue(&mut self.rt[index], id);
                if self.rt[index].is_empty() {
                    self.rt_bitmap &= !(1_u128 << index);
                }
                removed
            }
            QueueMembershipClass::Fair => self.fair.remove(id),
            QueueMembershipClass::IdleFair => self.idle_fair.remove(id),
        }
        .expect("runqueue membership must identify a linked scheduling entity");
        self.len -= 1;
        self.unregister_membership(removed.id);
        if matches!(removed.policy, SchedulePolicy::Deadline(_)) {
            self.recompute_earliest_deadline_event();
        }
        if self
            .pushable_summary
            .is_some_and(|summary| summary.thread == removed.id)
        {
            self.recompute_pushable_summary();
        }
        Some(removed)
    }

    pub(crate) fn detach_for_transfer(&mut self, id: ThreadId) -> Option<DetachedQueueEntry> {
        let class = self.membership_class(id)?;
        let (thread, restore_point) = match class {
            QueueMembershipClass::Deadline => {
                let position = self.deadline.iter().position(|thread| thread.id == id)?;
                (
                    self.deadline.remove(position),
                    QueueRestorePoint::Deadline(position),
                )
            }
            QueueMembershipClass::Realtime(priority) => {
                let index = (priority - 1) as usize;
                let position = self.rt[index].iter().position(|thread| thread.id == id)?;
                let thread = self.rt[index]
                    .remove(position)
                    .expect("indexed RT transfer entry must remain linked");
                if self.rt[index].is_empty() {
                    self.rt_bitmap &= !(1_u128 << index);
                }
                (thread, QueueRestorePoint::Realtime { priority, position })
            }
            QueueMembershipClass::Fair => (self.fair.remove(id)?, QueueRestorePoint::Fair),
            QueueMembershipClass::IdleFair => {
                (self.idle_fair.remove(id)?, QueueRestorePoint::IdleFair)
            }
        };
        self.len -= 1;
        self.unregister_membership(thread.id);
        if matches!(class, QueueMembershipClass::Deadline) {
            self.recompute_earliest_deadline_event();
        }
        if self
            .pushable_summary
            .is_some_and(|summary| summary.thread == thread.id)
        {
            self.recompute_pushable_summary();
        }
        Some(DetachedQueueEntry {
            thread,
            restore_point,
        })
    }

    pub(crate) fn restore_detached(&mut self, detached: DetachedQueueEntry) {
        let DetachedQueueEntry {
            thread,
            restore_point,
        } = detached;
        let id = thread.id;
        assert!(
            !self.contains(id),
            "a detached transfer entry must not already be queued"
        );
        let membership_class = match restore_point {
            QueueRestorePoint::Deadline(position) => {
                self.deadline
                    .insert(position.min(self.deadline.len()), thread);
                QueueMembershipClass::Deadline
            }
            QueueRestorePoint::Realtime { priority, position } => {
                let index = (priority - 1) as usize;
                self.rt[index].insert(position.min(self.rt[index].len()), thread);
                self.rt_bitmap |= 1_u128 << index;
                QueueMembershipClass::Realtime(priority)
            }
            QueueRestorePoint::Fair => {
                self.fair.insert(thread);
                QueueMembershipClass::Fair
            }
            QueueRestorePoint::IdleFair => {
                self.idle_fair.insert(thread);
                QueueMembershipClass::IdleFair
            }
        };
        self.len += 1;
        self.register_membership(id, membership_class);
        if matches!(membership_class, QueueMembershipClass::Deadline) {
            self.recompute_earliest_deadline_event();
        }
        self.recompute_pushable_summary();
    }

    pub(crate) fn pick_next_with_rt(
        &mut self,
        ordinary_rt_may_run: bool,
        mut is_pi_boosted_owner: impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThread> {
        let picked = self
            .pick_deadline()
            .or_else(|| self.pick_rt(ordinary_rt_may_run, &mut is_pi_boosted_owner))
            .or_else(|| self.pick_fair(false))
            .or_else(|| self.pick_fair(true));
        if let Some(picked_entry) = &picked {
            self.len -= 1;
            self.unregister_membership(picked_entry.id);
            if self
                .pushable_summary
                .is_some_and(|summary| summary.thread == picked_entry.id)
            {
                self.recompute_pushable_summary();
            }
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
        is_pi_boosted_owner: &mut impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThread> {
        if ordinary_rt_may_run {
            let priority = self.highest_rt_priority()?;
            let index = (priority - 1) as usize;
            let thread = self.rt[index]
                .pop_front()
                .expect("RT bitmap must identify a non-empty priority queue");
            if self.rt[index].is_empty() {
                self.rt_bitmap &= !(1_u128 << index);
            }
            return Some(thread);
        }
        for (index, queue) in self.rt.iter_mut().enumerate().rev() {
            if let Some(position) = queue.iter().position(&mut *is_pi_boosted_owner) {
                let thread = queue.remove(position);
                if queue.is_empty() {
                    self.rt_bitmap &= !(1_u128 << index);
                }
                if thread.is_some() {
                    return thread;
                }
            }
        }
        None
    }

    fn pick_fair(&mut self, idle: bool) -> Option<QueuedThread> {
        self.update_fair_virtual_time(None);
        let virtual_time = if idle {
            self.idle_virtual_time
        } else {
            self.virtual_time
        };
        let queue = if idle {
            &mut self.idle_fair
        } else {
            &mut self.fair
        };
        if queue.is_empty() {
            return None;
        }
        queue.pick_eligible(virtual_time)
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

    fn pushable_summary_for(thread: &QueuedThread) -> Option<PushableSummary> {
        (!matches!(
            thread.policy,
            SchedulePolicy::Fair {
                mode: FairMode::Idle,
                ..
            }
        ))
        .then(|| PushableSummary {
            thread: thread.id,
            key: thread.balance_key(),
        })
    }

    fn consider_pushable_summary(&mut self, candidate: Option<PushableSummary>) {
        if let Some(candidate) = candidate
            && self
                .pushable_summary
                .is_none_or(|current| candidate.key < current.key)
        {
            self.pushable_summary = Some(candidate);
        }
    }

    fn recompute_pushable_summary(&mut self) {
        self.pushable_summary = self
            .deadline
            .iter()
            .filter_map(Self::pushable_summary_for)
            .min_by_key(|summary| summary.key);
        let rt = self.highest_rt_priority().and_then(|priority| {
            self.rt[(priority - 1) as usize]
                .front()
                .and_then(Self::pushable_summary_for)
        });
        self.consider_pushable_summary(rt);
        let fair = self.fair.first().and_then(Self::pushable_summary_for);
        self.consider_pushable_summary(fair);
    }

    fn contains(&self, id: ThreadId) -> bool {
        self.membership_class(id).is_some()
    }

    fn membership_class(&self, id: ThreadId) -> Option<QueueMembershipClass> {
        #[cfg(test)]
        RUNQUEUE_MEMBERSHIP_LOOKUPS.set(RUNQUEUE_MEMBERSHIP_LOOKUPS.get().saturating_add(1));
        self.membership
            .get(id.slot() as usize)
            .and_then(|membership| *membership)
            .filter(|membership| membership.generation == id.generation())
            .map(|membership| membership.class)
    }

    fn register_membership(&mut self, id: ThreadId, class: QueueMembershipClass) {
        let slot = id.slot() as usize;
        if self.membership.len() <= slot {
            self.membership.resize(slot.saturating_add(1), None);
        }
        assert!(
            self.membership[slot]
                .replace(QueueMembership {
                    generation: id.generation(),
                    class,
                })
                .is_none(),
            "runqueue membership must be unique"
        );
    }

    fn unregister_membership(&mut self, id: ThreadId) {
        let membership = self
            .membership
            .get_mut(id.slot() as usize)
            .and_then(Option::take)
            .expect("queued thread must retain owner membership until removal");
        assert_eq!(membership.generation, id.generation());
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

fn remove_from_rt_queue(queue: &mut VecDeque<QueuedThread>, id: ThreadId) -> Option<QueuedThread> {
    let index = queue.iter().position(|entry| entry.id == id)?;
    queue.remove(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeadlineFlags, DeadlinePolicy, FairEntity, FairMode, Nice, RtPriority};

    #[test]
    fn deadline_precedes_rt_and_fair() {
        let mut queue = RunQueue::new();
        let fair = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let rt = SchedulePolicy::fifo(RtPriority::new(99).unwrap());
        let deadline =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
        queue
            .enqueue_test(
                ThreadId::from_parts(0, 1),
                fair,
                SchedulingEntity::new(fair, 1, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
        queue
            .enqueue_test(
                ThreadId::from_parts(1, 1),
                rt,
                SchedulingEntity::new(rt, 1, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
        queue
            .enqueue_test(
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
                .enqueue_test(
                    ThreadId::from_parts(slot, 1),
                    policy,
                    SchedulingEntity::new(policy, 1, 0),
                    0,
                    EnqueueReason::Wake,
                )
                .unwrap();
        }
        queue
            .enqueue_test(
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
            .enqueue_test(
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
    fn fair_yield_forfeits_request_before_positive_lag_peer() {
        let mut queue = RunQueue::new();
        let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let yielding = ThreadId::from_parts(0, 1);
        let waiting = ThreadId::from_parts(1, 1);

        queue
            .enqueue_test(
                waiting,
                policy,
                SchedulingEntity::new(policy, 100, 100),
                0,
                EnqueueReason::Migrated,
            )
            .unwrap();
        queue
            .enqueue_test(
                yielding,
                policy,
                SchedulingEntity::new(policy, 100, 0),
                0,
                EnqueueReason::Yield,
            )
            .unwrap();

        assert_eq!(
            queue.pick_next_with_rt(true, |_| false).unwrap().id,
            waiting,
            "yield must forfeit the active request so positive-lag peers become eligible",
        );
    }

    #[test]
    fn weighted_virtual_time_makes_every_non_negative_lag_entity_eligible() {
        let mut queue = RunQueue::new();
        let low_weight = SchedulePolicy::fair(Nice::new(19).unwrap(), FairMode::Normal);
        let normal_weight = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        for (slot, policy, vruntime, deadline) in [
            (0, low_weight, 0, 100),
            (1, normal_weight, 4, 8),
            (2, normal_weight, 10, 20),
        ] {
            let SchedulePolicy::Fair { nice, mode } = policy else {
                unreachable!();
            };
            queue
                .enqueue_test(
                    ThreadId::from_parts(slot, 1),
                    policy,
                    SchedulingEntity::Fair(FairEntity::test_state(nice, mode, vruntime, deadline)),
                    0,
                    EnqueueReason::Migrated,
                )
                .unwrap();
        }

        assert_eq!(
            queue.pick_next_with_rt(true, |_| false).unwrap().id,
            ThreadId::from_parts(1, 1),
            "weighted V must make both vruntime 0 and 4 eligible, then choose vd=8",
        );
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
            .enqueue_test(thread, policy, entity, 4, EnqueueReason::Preempted)
            .unwrap();

        let deadline = queue.dequeue(thread).unwrap().entity.deadline().unwrap();
        assert_eq!(deadline.absolute_deadline_ns(), 8);
        assert_eq!(deadline.remaining_runtime_ns(), 3);
    }

    #[test]
    fn pushable_summary_tracks_the_top_non_idle_thread() {
        let mut queue = RunQueue::new();
        let idle = SchedulePolicy::fair(Nice::ZERO, FairMode::Idle);
        let fair = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let rt = SchedulePolicy::fifo(RtPriority::new(80).unwrap());
        let deadline =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
        let idle_id = ThreadId::from_parts(0, 1);
        let fair_id = ThreadId::from_parts(1, 1);
        let rt_id = ThreadId::from_parts(2, 1);
        let deadline_id = ThreadId::from_parts(3, 1);

        queue
            .enqueue_test(
                idle_id,
                idle,
                SchedulingEntity::new(idle, 1, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
        assert_eq!(queue.pushable_key(), None);
        for (id, policy) in [(fair_id, fair), (rt_id, rt), (deadline_id, deadline)] {
            queue
                .enqueue_test(
                    id,
                    policy,
                    SchedulingEntity::new(policy, 1, 0),
                    0,
                    EnqueueReason::Wake,
                )
                .unwrap();
        }
        assert_eq!(queue.pushable_key().unwrap().class_rank(), 0);

        queue.dequeue(deadline_id).unwrap();
        assert_eq!(queue.pushable_key().unwrap().class_rank(), 1);
        assert_eq!(queue.pick_next_with_rt(true, |_| false).unwrap().id, rt_id);
        assert_eq!(queue.pushable_key().unwrap().class_rank(), 2);
        queue.dequeue(fair_id).unwrap();
        assert_eq!(queue.pushable_key(), None);
        assert_eq!(queue.dequeue(idle_id).unwrap().id, idle_id);
    }

    #[test]
    fn fair_virtual_time_and_pick_do_not_scan_the_runnable_set() {
        let mut queue = RunQueue::new();
        let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        for slot in 0..128 {
            queue
                .enqueue_test(
                    ThreadId::from_parts(slot, 1),
                    policy,
                    SchedulingEntity::new(policy, 1_000, slot as u64),
                    0,
                    EnqueueReason::Migrated,
                )
                .unwrap();
        }

        reset_fair_runqueue_visits();
        queue.update_fair_virtual_time(None);
        assert_eq!(
            fair_runqueue_visits(),
            0,
            "weighted virtual time must come from incrementally maintained rq sums"
        );

        queue.fair.assert_invariants();
        while queue.has_fair() {
            reset_fair_runqueue_visits();
            queue.pick_next_with_rt(true, |_| false).unwrap();
            assert!(
                fair_runqueue_visits() <= 32,
                "EEVDF selection must remain logarithmic, observed {} visits",
                fair_runqueue_visits()
            );
            queue.fair.assert_invariants();
        }

        let mut removal_queue = RunQueue::new();
        for slot in 0..128 {
            removal_queue
                .enqueue_test(
                    ThreadId::from_parts(slot, 1),
                    policy,
                    SchedulingEntity::new(policy, 1_000, slot as u64),
                    0,
                    EnqueueReason::Migrated,
                )
                .unwrap();
        }
        for index in 0..128 {
            let slot = (index * 73) % 128;
            removal_queue
                .dequeue(ThreadId::from_parts(slot, 1))
                .unwrap();
            removal_queue.fair.assert_invariants();
        }
    }

    #[test]
    fn fair_enqueue_uses_direct_runqueue_membership() {
        let mut queue = RunQueue::new();
        let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        queue
            .enqueue_test(
                ThreadId::from_parts(0, 1),
                policy,
                SchedulingEntity::new(policy, 1_000, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();

        reset_runqueue_membership_lookups();
        queue
            .enqueue_test(
                ThreadId::from_parts(1, 1),
                policy,
                SchedulingEntity::new(policy, 1_000, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
        assert_eq!(
            runqueue_membership_lookups(),
            1,
            "enqueue must perform one generation-checked lookup instead of probing scheduler \
             classes"
        );
    }

    #[test]
    fn direct_membership_rejects_a_retired_thread_generation() {
        let mut queue = RunQueue::new();
        let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let retired = ThreadId::from_parts(7, 1);
        let replacement = ThreadId::from_parts(7, 2);

        queue
            .enqueue_test(
                retired,
                policy,
                SchedulingEntity::new(policy, 1_000, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();
        assert_eq!(queue.dequeue(retired).unwrap().id, retired);
        queue
            .enqueue_test(
                replacement,
                policy,
                SchedulingEntity::new(policy, 1_000, 0),
                0,
                EnqueueReason::Wake,
            )
            .unwrap();

        assert!(queue.dequeue(retired).is_none());
        assert_eq!(queue.dequeue(replacement).unwrap().id, replacement);
    }

    #[test]
    fn realtime_bitmap_tracks_the_highest_nonempty_priority() {
        let mut queue = RunQueue::new();
        let low = SchedulePolicy::fifo(RtPriority::new(1).unwrap());
        let high = SchedulePolicy::fifo(RtPriority::new(99).unwrap());
        let low_id = ThreadId::from_parts(0, 1);
        let high_id = ThreadId::from_parts(1, 1);
        for (id, policy) in [(low_id, low), (high_id, high)] {
            queue
                .enqueue_test(
                    id,
                    policy,
                    SchedulingEntity::new(policy, 1_000, 0),
                    0,
                    EnqueueReason::Wake,
                )
                .unwrap();
        }

        assert_eq!(queue.highest_rt_priority(), Some(99));
        assert_eq!(queue.dequeue(high_id).unwrap().id, high_id);
        assert_eq!(queue.highest_rt_priority(), Some(1));
        assert_eq!(queue.pick_next_with_rt(true, |_| false).unwrap().id, low_id);
        assert!(!queue.has_rt());
    }
}
