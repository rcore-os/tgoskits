//! Fixed-priority FIFO/RR runqueue owned by the real-time scheduling class.

use alloc::{boxed::Box, sync::Arc};
use core::ptr::NonNull;

use super::{EnqueueReason, QueuedThread, QueuedThreadSnapshot};
use crate::{SchedulePolicy, SchedulingEntity, ThreadId};

const RT_PRIORITY_LEVELS: usize = 99;
const FIXED_PRIORITY_LEVELS: usize = RT_PRIORITY_LEVELS;
const RT_PRIORITY_BITMAP: u128 = (1_u128 << RT_PRIORITY_LEVELS) - 1;

#[cfg(test)]
std::thread_local! {
    static REALTIME_ACTIVE_ITER_VISITS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
    static REALTIME_PUSHABLE_ITER_VISITS: core::cell::Cell<usize> = const {
        core::cell::Cell::new(0)
    };
}

#[cfg(test)]
pub(super) fn reset_realtime_queue_visits() {
    REALTIME_ACTIVE_ITER_VISITS.set(0);
    REALTIME_PUSHABLE_ITER_VISITS.set(0);
}

#[cfg(test)]
pub(super) fn realtime_active_iter_visits() -> usize {
    REALTIME_ACTIVE_ITER_VISITS.get()
}

#[cfg(test)]
pub(super) fn realtime_pushable_iter_visits() -> usize {
    REALTIME_PUSHABLE_ITER_VISITS.get()
}

/// Per-thread RT linkage prepared during thread construction.
#[derive(Debug)]
pub(crate) struct RealtimeNode {
    thread: Option<QueuedThread>,
    next: Option<Box<RealtimeNode>>,
    pushable: bool,
}

impl RealtimeNode {
    pub(crate) fn empty() -> Box<Self> {
        Box::new(Self {
            thread: None,
            next: None,
            pushable: false,
        })
    }

    fn reset(&mut self, thread: QueuedThread) {
        self.thread = Some(thread);
        self.next = None;
        self.pushable = false;
    }

    fn thread(&self) -> &QueuedThread {
        self.thread
            .as_ref()
            .expect("linked RT node must own one scheduling entity")
    }

    fn thread_mut(&mut self) -> &mut QueuedThread {
        self.thread
            .as_mut()
            .expect("linked RT node must own one scheduling entity")
    }
}

/// Per-thread linkage for Linux `rt_rq::pushable_tasks`.
#[derive(Debug)]
pub(crate) struct RealtimePushableNode {
    thread: ThreadId,
    active: Option<NonNull<RealtimeNode>>,
    next: Option<Box<RealtimePushableNode>>,
}

impl RealtimePushableNode {
    pub(crate) fn empty() -> Box<Self> {
        Box::new(Self {
            thread: ThreadId::from_parts(0, 0),
            active: None,
            next: None,
        })
    }

    fn reset(&mut self, thread: ThreadId, active: NonNull<RealtimeNode>) {
        self.thread = thread;
        self.active = Some(active);
        self.next = None;
    }

    fn active(&self) -> &RealtimeNode {
        let active = self
            .active
            .expect("linked RT pushable node must identify its active node");
        unsafe {
            // SAFETY: both nodes are linked and accessed under the same owner
            // rq lock. The active node is Box-stable, and dequeue always
            // removes this pushable node before returning the active storage.
            active.as_ref()
        }
    }

    fn clear_active(&mut self) {
        self.active = None;
    }
}

// SAFETY: a non-null active link exists only while both task-owned nodes are
// linked to the same owner rq. Placement and the rq lock serialize every
// access, and the link is cleared before the node returns to task storage.
unsafe impl Send for RealtimePushableNode {}

#[derive(Debug)]
struct RealtimeLevel {
    head: Option<Box<RealtimeNode>>,
    tail: Option<NonNull<RealtimeNode>>,
    len: usize,
}

// SAFETY: `tail` points into the Box chain owned by `head`. The complete level
// is moved only while its enclosing runqueue is exclusively owned.
unsafe impl Send for RealtimeLevel {}

impl RealtimeLevel {
    const fn new() -> Self {
        Self {
            head: None,
            tail: None,
            len: 0,
        }
    }

    fn push_front(&mut self, mut node: Box<RealtimeNode>) {
        if self.head.is_none() {
            self.tail = Some(NonNull::from(node.as_mut()));
        }
        node.next = self.head.take();
        self.head = Some(node);
        self.len += 1;
    }

    fn push_back(&mut self, mut node: Box<RealtimeNode>) {
        let node_pointer = NonNull::from(node.as_mut());
        match self.tail {
            Some(mut tail) => unsafe {
                // SAFETY: `tail` is the last node of the Box chain owned by
                // this level and the runqueue lock provides unique access.
                tail.as_mut().next = Some(node);
            },
            None => self.head = Some(node),
        }
        self.tail = Some(node_pointer);
        self.len += 1;
    }

    fn remove_at(&mut self, position: usize) -> Option<Box<RealtimeNode>> {
        if position >= self.len {
            return None;
        }
        let mut previous = None;
        let mut link = &mut self.head;
        for _ in 0..position {
            let node = link.as_mut()?;
            previous = Some(NonNull::from(node.as_mut()));
            link = &mut node.next;
        }
        let mut removed = link.take()?;
        *link = removed.next.take();
        self.len -= 1;
        if self.tail == Some(NonNull::from(removed.as_mut())) {
            self.tail = previous;
        }
        if self.head.is_none() {
            self.tail = None;
        }
        Some(removed)
    }

    fn position(&self, id: ThreadId) -> Option<usize> {
        self.iter().position(|thread| thread.id == id)
    }

    fn iter(&self) -> RealtimeIter<'_> {
        RealtimeIter {
            next: self.head.as_deref(),
        }
    }
}

impl Drop for RealtimeLevel {
    fn drop(&mut self) {
        while let Some(mut node) = self.head.take() {
            self.head = node.next.take();
        }
        self.tail = None;
        self.len = 0;
    }
}

struct RealtimeIter<'queue> {
    next: Option<&'queue RealtimeNode>,
}

#[derive(Debug)]
struct RealtimePushableLevel {
    head: Option<Box<RealtimePushableNode>>,
    tail: Option<NonNull<RealtimePushableNode>>,
    len: usize,
}

// SAFETY: `tail` points into the Box chain owned by `head`, and the complete
// pushable list moves only with its exclusively owned rq.
unsafe impl Send for RealtimePushableLevel {}

impl RealtimePushableLevel {
    const fn new() -> Self {
        Self {
            head: None,
            tail: None,
            len: 0,
        }
    }

    fn push_back(&mut self, mut node: Box<RealtimePushableNode>) {
        let node_pointer = NonNull::from(node.as_mut());
        match self.tail {
            Some(mut tail) => unsafe {
                // SAFETY: `tail` is the final node in the chain owned by this
                // list and the rq lock provides unique access.
                tail.as_mut().next = Some(node);
            },
            None => self.head = Some(node),
        }
        self.tail = Some(node_pointer);
        self.len += 1;
    }

    fn position(&self, thread: ThreadId) -> Option<usize> {
        self.iter().position(|candidate| candidate.thread == thread)
    }

    fn remove_at(&mut self, position: usize) -> Option<Box<RealtimePushableNode>> {
        if position >= self.len {
            return None;
        }
        let mut previous = None;
        let mut link = &mut self.head;
        for _ in 0..position {
            let node = link.as_mut()?;
            previous = Some(NonNull::from(node.as_mut()));
            link = &mut node.next;
        }
        let mut removed = link.take()?;
        *link = removed.next.take();
        self.len -= 1;
        if self.tail == Some(NonNull::from(removed.as_mut())) {
            self.tail = previous;
        }
        if self.head.is_none() {
            self.tail = None;
        }
        Some(removed)
    }

    fn iter(&self) -> RealtimePushableIter<'_> {
        RealtimePushableIter {
            next: self.head.as_deref(),
        }
    }
}

impl Drop for RealtimePushableLevel {
    fn drop(&mut self) {
        while let Some(mut node) = self.head.take() {
            self.head = node.next.take();
        }
        self.tail = None;
        self.len = 0;
    }
}

struct RealtimePushableIter<'queue> {
    next: Option<&'queue RealtimePushableNode>,
}

impl<'queue> Iterator for RealtimePushableIter<'queue> {
    type Item = &'queue RealtimePushableNode;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.next?;
        self.next = node.next.as_deref();
        #[cfg(test)]
        REALTIME_PUSHABLE_ITER_VISITS.set(REALTIME_PUSHABLE_ITER_VISITS.get().saturating_add(1));
        Some(node)
    }
}

impl<'queue> Iterator for RealtimeIter<'queue> {
    type Item = &'queue QueuedThread;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.next?;
        self.next = node.next.as_deref();
        #[cfg(test)]
        REALTIME_ACTIVE_ITER_VISITS.set(REALTIME_ACTIVE_ITER_VISITS.get().saturating_add(1));
        Some(node.thread())
    }
}

/// Linux-style RT priority array: one intrusive FIFO per priority plus cached
/// bitmaps. Queue nodes are embedded scheduler storage; enqueue/dequeue never
/// allocate or free memory while the rq lock is held.
#[derive(Debug)]
pub(super) struct RealtimeRunQueue {
    active: [RealtimeLevel; FIXED_PRIORITY_LEVELS],
    active_bitmap: u128,
    exempt_bitmap: u128,
    exempt_count: [usize; FIXED_PRIORITY_LEVELS],
    pushable: [RealtimePushableLevel; FIXED_PRIORITY_LEVELS],
    pushable_bitmap: u128,
}

impl RealtimeRunQueue {
    pub(super) fn new() -> Self {
        Self {
            active: core::array::from_fn(|_| RealtimeLevel::new()),
            active_bitmap: 0,
            exempt_bitmap: 0,
            exempt_count: [0; FIXED_PRIORITY_LEVELS],
            pushable: core::array::from_fn(|_| RealtimePushableLevel::new()),
            pushable_bitmap: 0,
        }
    }

    pub(super) const fn has_any_rt(&self) -> bool {
        self.active_bitmap & RT_PRIORITY_BITMAP != 0
    }

    pub(super) const fn has_exempt_rt(&self) -> bool {
        self.exempt_bitmap & RT_PRIORITY_BITMAP != 0
    }

    pub(super) fn highest_rt_priority(&self) -> Option<u8> {
        bitmap_highest_priority(self.active_bitmap & RT_PRIORITY_BITMAP)
    }

    pub(super) const fn has_pushable(&self) -> bool {
        self.pushable_bitmap & RT_PRIORITY_BITMAP != 0
    }

    pub(super) fn pushable_count(&self) -> usize {
        self.pushable.iter().map(|level| level.len).sum()
    }

    pub(super) fn refresh_pushable(
        &mut self,
        thread: ThreadId,
        priority: u8,
        current: Option<ThreadId>,
    ) {
        let index = (priority - 1) as usize;
        let Some(position) = self.active[index].position(thread) else {
            return;
        };
        let (should_be_pushable, is_pushable, core, active) = {
            let node = self.active[index]
                .head
                .as_deref()
                .and_then(|head| nth_node(head, position))
                .expect("RT position must identify one linked node");
            (
                node.thread().migration_capable && current != Some(thread),
                node.pushable,
                Arc::clone(&node.thread().core),
                NonNull::from(node),
            )
        };
        match (is_pushable, should_be_pushable) {
            (false, true) => {
                let mut node = unsafe {
                    // SAFETY: the task is linked to this rq and the owner rq
                    // lock serializes its independent pushable membership.
                    core.runqueue_nodes().take_realtime_pushable()
                };
                node.reset(thread, active);
                self.pushable[index].push_back(node);
                self.active[index]
                    .head
                    .as_deref_mut()
                    .and_then(|head| nth_node_mut(head, position))
                    .expect("RT position must remain stable under the rq lock")
                    .pushable = true;
            }
            (true, false) => {
                let pushable_position = self.pushable[index]
                    .position(thread)
                    .expect("RT pushable flag must match its priority list");
                let mut node = self.pushable[index]
                    .remove_at(pushable_position)
                    .expect("RT pushable position must remain linked");
                debug_assert_eq!(node.active, Some(active));
                node.clear_active();
                self.active[index]
                    .head
                    .as_deref_mut()
                    .and_then(|head| nth_node_mut(head, position))
                    .expect("RT position must remain stable under the rq lock")
                    .pushable = false;
                unsafe {
                    // SAFETY: the node is detached from the pushable list
                    // and no longer contains an active-node link before it
                    // returns to task-owned storage.
                    core.runqueue_nodes().return_realtime_pushable(node);
                }
            }
            _ => {}
        }
        let bit = 1_u128 << index;
        if self.pushable[index].len != 0 {
            self.pushable_bitmap |= bit;
        } else {
            self.pushable_bitmap &= !bit;
        }
    }

    pub(super) fn count_at_priority(&self, priority: u8) -> usize {
        priority
            .checked_sub(1)
            .and_then(|index| self.active.get(index as usize))
            .map_or(0, |level| level.len)
    }

    pub(super) fn enqueue(&mut self, thread: QueuedThread, reason: EnqueueReason) -> u8 {
        let priority = thread
            .active
            .policy()
            .rt_priority()
            .expect("RT priority array requires FIFO or RR policy")
            .get();
        let index = (priority - 1) as usize;
        if thread.rt_quota_exempt {
            self.exempt_count[index] = self.exempt_count[index].saturating_add(1);
            self.exempt_bitmap |= 1_u128 << index;
        }
        let core = Arc::clone(&thread.core);
        let mut node = unsafe {
            // SAFETY: the placement state and target rq lock serialize the
            // only RT linkage belonging to this thread.
            core.runqueue_nodes().take_realtime()
        };
        node.reset(thread);
        if reason == EnqueueReason::Preempted {
            self.active[index].push_front(node);
        } else {
            self.active[index].push_back(node);
        }
        self.active_bitmap |= 1_u128 << index;
        priority
    }

    pub(super) fn remove(&mut self, priority: u8, id: ThreadId) -> Option<QueuedThread> {
        let index = (priority - 1) as usize;
        let position = self.active[index].position(id)?;
        if self.active[index]
            .head
            .as_deref()
            .and_then(|head| nth_node(head, position))
            .is_some_and(|node| node.pushable)
        {
            self.refresh_pushable(id, priority, Some(id));
        }
        let node = self.active[index].remove_at(position)?;
        Some(self.after_remove(index, node))
    }

    pub(super) fn get(&self, priority: u8, id: ThreadId) -> Option<&QueuedThread> {
        self.active[(priority - 1) as usize]
            .iter()
            .find(|thread| thread.id == id)
    }

    pub(super) fn get_mut(&mut self, priority: u8, id: ThreadId) -> Option<&mut QueuedThread> {
        let mut node = self.active[(priority - 1) as usize].head.as_deref_mut();
        while let Some(current) = node {
            if current.thread().id == id {
                return Some(current.thread_mut());
            }
            node = current.next.as_deref_mut();
        }
        None
    }

    pub(super) fn find_first_pushable_matching(
        &self,
        predicate: &mut impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThreadSnapshot> {
        self.pushable
            .iter()
            .enumerate()
            .take(RT_PRIORITY_LEVELS)
            .rev()
            .find_map(|(index, level)| {
                level.iter().find_map(|pushable| {
                    let active = pushable.active();
                    debug_assert_eq!(active.thread().id, pushable.thread);
                    debug_assert_eq!(
                        active
                            .thread()
                            .active
                            .policy()
                            .rt_priority()
                            .expect("RT active node must retain a fixed priority")
                            .get() as usize,
                        index + 1,
                    );
                    predicate(active.thread()).then(|| QueuedThreadSnapshot::from(active.thread()))
                })
            })
    }

    pub(super) fn select(&self) -> Option<QueuedThreadSnapshot> {
        let priority = self.highest_rt_priority()?;
        let index = (priority - 1) as usize;
        self.active[index]
            .iter()
            .next()
            .map(QueuedThreadSnapshot::from)
    }

    pub(super) fn put_prev_current(
        &mut self,
        priority: u8,
        id: ThreadId,
        reason: EnqueueReason,
    ) -> Option<SchedulingEntity> {
        let index = (priority - 1) as usize;
        let position = self.active[index].position(id)?;
        let move_to_tail = matches!(reason, EnqueueReason::Yield);
        if move_to_tail {
            let node = self.active[index].remove_at(position)?;
            let entity = node.thread().active.entity().clone();
            self.active[index].push_back(node);
            Some(entity)
        } else {
            self.active[index]
                .iter()
                .nth(position)
                .map(QueuedThread::entity_snapshot)
        }
    }

    /// Linux `task_tick_rt()` for one linked RR current.
    ///
    /// The current task stays in the active priority array.  Expiration
    /// refreshes its quantum unconditionally; only a peer at the same
    /// priority causes `requeue_task_rt()` and a reschedule request.
    pub(super) fn task_tick_round_robin(
        &mut self,
        priority: u8,
        id: ThreadId,
        policy: SchedulePolicy,
    ) -> Option<bool> {
        let index = (priority - 1) as usize;
        let position = self.active[index].position(id)?;
        let expired = self.active[index]
            .iter()
            .nth(position)?
            .active
            .entity()
            .round_robin_quantum_expired();
        if !expired {
            return Some(false);
        }

        let has_peer = self.active[index].len > 1;
        if has_peer {
            let mut node = self.active[index].remove_at(position)?;
            node.thread_mut()
                .active
                .entity_mut()
                .reset_round_robin_quantum(policy);
            self.active[index].push_back(node);
        } else {
            self.active[index]
                .head
                .as_deref_mut()?
                .thread_mut()
                .active
                .entity_mut()
                .reset_round_robin_quantum(policy);
        }
        Some(has_peer)
    }

    fn after_remove(&mut self, index: usize, mut node: Box<RealtimeNode>) -> QueuedThread {
        assert!(
            !node.pushable,
            "RT active node must leave its pushable list before dequeue"
        );
        let thread = node
            .thread
            .take()
            .expect("removed RT node must retain its scheduling entity");
        if thread.rt_quota_exempt {
            self.exempt_count[index] -= 1;
            if self.exempt_count[index] == 0 {
                self.exempt_bitmap &= !(1_u128 << index);
            }
        }
        if self.active[index].len == 0 {
            self.active_bitmap &= !(1_u128 << index);
            debug_assert_eq!(self.exempt_count[index], 0);
            self.exempt_bitmap &= !(1_u128 << index);
            self.pushable_bitmap &= !(1_u128 << index);
        }
        unsafe {
            // SAFETY: the node is no longer linked and placement prevents a
            // concurrent enqueue until this rq transaction returns it.
            thread.core.runqueue_nodes().return_realtime(node);
        }
        thread
    }
}

fn nth_node(mut node: &RealtimeNode, mut position: usize) -> Option<&RealtimeNode> {
    while position != 0 {
        node = node.next.as_deref()?;
        position -= 1;
    }
    Some(node)
}

fn nth_node_mut(mut node: &mut RealtimeNode, mut position: usize) -> Option<&mut RealtimeNode> {
    while position != 0 {
        node = node.next.as_deref_mut()?;
        position -= 1;
    }
    Some(node)
}

fn bitmap_highest_priority(bitmap: u128) -> Option<u8> {
    (bitmap != 0).then(|| (u128::BITS - bitmap.leading_zeros()) as u8)
}
