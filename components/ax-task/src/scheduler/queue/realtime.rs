//! Fixed-priority FIFO/RR runqueue owned by the real-time scheduling class.

use alloc::{boxed::Box, sync::Arc};
use core::{fmt, ptr::NonNull};

use super::{EnqueueReason, LinkedPickedThread, QueuedThread, QueuedThreadSnapshot};
use crate::{SchedulePolicy, SchedulingEntity, ThreadId};

const RT_PRIORITY_LEVELS: usize = 99;
const FIXED_PRIORITY_LEVELS: usize = RT_PRIORITY_LEVELS;
const RT_PRIORITY_BITMAP: u128 = (1_u128 << RT_PRIORITY_LEVELS) - 1;

/// Per-thread RT linkage prepared during thread construction.
#[derive(Debug)]
pub(crate) struct RealtimeNode {
    thread: Option<QueuedThread>,
    prev: Option<NonNull<RealtimeNode>>,
    next: Option<Box<RealtimeNode>>,
    pushable: Option<NonNull<RealtimePushableNode>>,
}

impl RealtimeNode {
    pub(crate) fn empty() -> Box<Self> {
        Box::new(Self {
            thread: None,
            prev: None,
            next: None,
            pushable: None,
        })
    }

    fn reset(&mut self, thread: QueuedThread) {
        self.thread = Some(thread);
        self.prev = None;
        self.next = None;
        self.pushable = None;
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

// SAFETY: raw list links are non-null only while task-owned storage is linked
// to one owner rq. The rq lock serializes access, and unlink clears both links
// before the Box can move between the rq and per-thread storage.
unsafe impl Send for RealtimeNode {}

/// Per-thread linkage for Linux `rt_rq::pushable_tasks`.
#[derive(Debug)]
pub(crate) struct RealtimePushableNode {
    thread: ThreadId,
    active: Option<NonNull<RealtimeNode>>,
    prev: Option<NonNull<RealtimePushableNode>>,
    next: Option<Box<RealtimePushableNode>>,
}

impl RealtimePushableNode {
    pub(crate) fn empty() -> Box<Self> {
        Box::new(Self {
            thread: ThreadId::from_parts(0, 0),
            active: None,
            prev: None,
            next: None,
        })
    }

    fn reset(&mut self, thread: ThreadId, active: NonNull<RealtimeNode>) {
        self.thread = thread;
        self.active = Some(active);
        self.prev = None;
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

/// Stable identity of one task-owned node while it is linked to an RT rq.
///
/// Linux keeps the equivalent identity in the task's embedded `run_list`.
/// The key is published only in the owner rq's membership table and is
/// invalidated before the detached Box returns to task storage.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct RealtimeQueueKey {
    priority: u8,
    thread: ThreadId,
    node: NonNull<RealtimeNode>,
}

impl RealtimeQueueKey {
    const fn index(self) -> usize {
        (self.priority - 1) as usize
    }
}

impl fmt::Debug for RealtimeQueueKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RealtimeQueueKey")
            .field("priority", &self.priority)
            .field("thread", &self.thread)
            .finish_non_exhaustive()
    }
}

// SAFETY: the key is only dereferenced by `RealtimeRunQueue` while the owner
// rq lock keeps the task-owned Box linked and immobile. Copying or moving the
// opaque key itself does not access the pointee.
unsafe impl Send for RealtimeQueueKey {}
// SAFETY: sharing the opaque identity does not grant access to the pointee;
// all dereferences remain serialized by the owner rq lock.
unsafe impl Sync for RealtimeQueueKey {}

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

    fn push_front(&mut self, mut node: Box<RealtimeNode>) -> NonNull<RealtimeNode> {
        debug_assert!(node.prev.is_none());
        debug_assert!(node.next.is_none());
        let node_pointer = NonNull::from(node.as_mut());
        node.next = self.head.take();
        if let Some(next) = node.next.as_deref_mut() {
            next.prev = Some(node_pointer);
        } else {
            self.tail = Some(NonNull::from(node.as_mut()));
        }
        self.head = Some(node);
        self.len += 1;
        node_pointer
    }

    fn push_back(&mut self, mut node: Box<RealtimeNode>) -> NonNull<RealtimeNode> {
        debug_assert!(node.prev.is_none());
        debug_assert!(node.next.is_none());
        let node_pointer = NonNull::from(node.as_mut());
        node.prev = self.tail;
        match self.tail {
            Some(mut tail) => unsafe {
                // SAFETY: `tail` is the last node of the Box chain owned by
                // this level and the runqueue lock provides unique access.
                debug_assert!(tail.as_ref().next.is_none());
                tail.as_mut().next = Some(node);
            },
            None => self.head = Some(node),
        }
        self.tail = Some(node_pointer);
        self.len += 1;
        node_pointer
    }

    fn remove(&mut self, node_pointer: NonNull<RealtimeNode>) -> Option<Box<RealtimeNode>> {
        let previous = unsafe {
            // SAFETY: callers supply a key published for a node linked to this
            // owner rq, whose lock prevents concurrent removal.
            node_pointer.as_ref().prev
        };
        let mut removed = match previous {
            Some(mut previous) => unsafe {
                // SAFETY: `previous` and `node_pointer` are adjacent nodes in
                // this Box-owned chain while the rq lock provides exclusivity.
                let link = &mut previous.as_mut().next;
                if !link
                    .as_deref()
                    .is_some_and(|node| NonNull::from(node) == node_pointer)
                {
                    return None;
                }
                let mut removed = link.take()?;
                *link = removed.next.take();
                if let Some(next) = link.as_deref_mut() {
                    next.prev = Some(previous);
                } else {
                    self.tail = Some(previous);
                }
                removed
            },
            None => {
                if !self
                    .head
                    .as_deref()
                    .is_some_and(|node| NonNull::from(node) == node_pointer)
                {
                    return None;
                }
                let mut removed = self.head.take()?;
                self.head = removed.next.take();
                if let Some(head) = self.head.as_deref_mut() {
                    head.prev = None;
                } else {
                    self.tail = None;
                }
                removed
            }
        };
        removed.prev = None;
        removed.next = None;
        self.len -= 1;
        if self.head.is_none() {
            self.tail = None;
        }
        Some(removed)
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

    fn push_back(&mut self, mut node: Box<RealtimePushableNode>) -> NonNull<RealtimePushableNode> {
        debug_assert!(node.prev.is_none());
        debug_assert!(node.next.is_none());
        let node_pointer = NonNull::from(node.as_mut());
        node.prev = self.tail;
        match self.tail {
            Some(mut tail) => unsafe {
                // SAFETY: `tail` is the final node in the chain owned by this
                // list and the rq lock provides unique access.
                debug_assert!(tail.as_ref().next.is_none());
                tail.as_mut().next = Some(node);
            },
            None => self.head = Some(node),
        }
        self.tail = Some(node_pointer);
        self.len += 1;
        node_pointer
    }

    fn remove(
        &mut self,
        node_pointer: NonNull<RealtimePushableNode>,
    ) -> Option<Box<RealtimePushableNode>> {
        let previous = unsafe {
            // SAFETY: the active RT node records this list member while both
            // nodes remain linked under the same owner rq lock.
            node_pointer.as_ref().prev
        };
        let mut removed = match previous {
            Some(mut previous) => unsafe {
                // SAFETY: `previous` and `node_pointer` are adjacent nodes in
                // this Box-owned chain and the rq lock provides exclusivity.
                let link = &mut previous.as_mut().next;
                if !link
                    .as_deref()
                    .is_some_and(|node| NonNull::from(node) == node_pointer)
                {
                    return None;
                }
                let mut removed = link.take()?;
                *link = removed.next.take();
                if let Some(next) = link.as_deref_mut() {
                    next.prev = Some(previous);
                } else {
                    self.tail = Some(previous);
                }
                removed
            },
            None => {
                if !self
                    .head
                    .as_deref()
                    .is_some_and(|node| NonNull::from(node) == node_pointer)
                {
                    return None;
                }
                let mut removed = self.head.take()?;
                self.head = removed.next.take();
                if let Some(head) = self.head.as_deref_mut() {
                    head.prev = None;
                } else {
                    self.tail = None;
                }
                removed
            }
        };
        removed.prev = None;
        removed.next = None;
        self.len -= 1;
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
        Some(node)
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

    pub(super) fn refresh_pushable(&mut self, key: RealtimeQueueKey, current: Option<ThreadId>) {
        let index = key.index();
        let Some(node) = self.node(key) else {
            return;
        };
        let should_be_pushable = node.thread().migration_capable && current != Some(key.thread);
        let is_pushable = node.pushable.is_some();
        match (is_pushable, should_be_pushable) {
            (false, true) => {
                let core = Arc::clone(
                    &self
                        .node(key)
                        .expect("RT key must remain linked")
                        .thread()
                        .core,
                );
                let mut node = unsafe {
                    // SAFETY: the task is linked to this rq and the owner rq
                    // lock serializes its independent pushable membership.
                    core.runqueue_nodes().take_realtime_pushable()
                };
                node.reset(key.thread, key.node);
                let pushable = self.pushable[index].push_back(node);
                let mut active = key.node;
                unsafe {
                    // SAFETY: the key remains linked to this rq and the owner
                    // lock grants unique access to its membership fields.
                    active.as_mut().pushable = Some(pushable);
                }
            }
            (true, false) => {
                let pushable = self
                    .node(key)
                    .expect("RT key must remain linked")
                    .pushable
                    .expect("RT pushable link must identify its priority list");
                let core = Arc::clone(
                    &self
                        .node(key)
                        .expect("RT key must remain linked")
                        .thread()
                        .core,
                );
                let mut node = self.pushable[index]
                    .remove(pushable)
                    .expect("RT pushable key must remain linked");
                debug_assert_eq!(node.active, Some(key.node));
                node.clear_active();
                let mut active = key.node;
                unsafe {
                    // SAFETY: the active key remains linked and the detached
                    // pushable node is no longer reachable from the rq.
                    active.as_mut().pushable = None;
                }
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

    pub(super) fn enqueue(
        &mut self,
        thread: QueuedThread,
        reason: EnqueueReason,
    ) -> RealtimeQueueKey {
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
        let node = if reason == EnqueueReason::Preempted {
            self.active[index].push_front(node)
        } else {
            self.active[index].push_back(node)
        };
        self.active_bitmap |= 1_u128 << index;
        RealtimeQueueKey {
            priority,
            thread: unsafe {
                // SAFETY: the node was just linked to this rq and remains
                // Box-stable until the returned key is invalidated.
                node.as_ref().thread().id
            },
            node,
        }
    }

    pub(super) fn remove(&mut self, key: RealtimeQueueKey) -> Option<QueuedThread> {
        let index = key.index();
        if self.node(key)?.pushable.is_some() {
            self.refresh_pushable(key, Some(key.thread));
        }
        let node = self.active[index].remove(key.node)?;
        Some(self.after_remove(index, node))
    }

    pub(super) fn get(&self, key: RealtimeQueueKey) -> Option<&QueuedThread> {
        self.node(key).map(RealtimeNode::thread)
    }

    pub(super) fn get_mut(&mut self, key: RealtimeQueueKey) -> Option<&mut QueuedThread> {
        self.node_mut(key).map(RealtimeNode::thread_mut)
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

    pub(super) fn select(&self) -> Option<LinkedPickedThread> {
        let priority = self.highest_rt_priority()?;
        let index = (priority - 1) as usize;
        self.active[index]
            .head
            .as_deref()
            .map(RealtimeNode::thread)
            .map(LinkedPickedThread::from)
    }

    pub(super) fn put_prev_current(
        &mut self,
        key: RealtimeQueueKey,
        reason: EnqueueReason,
    ) -> Option<SchedulingEntity> {
        let index = key.index();
        let move_to_tail = matches!(reason, EnqueueReason::Yield);
        if move_to_tail {
            let node = self.active[index].remove(key.node)?;
            let entity = node.thread().active.entity().clone();
            let requeued = self.active[index].push_back(node);
            debug_assert_eq!(requeued, key.node);
            Some(entity)
        } else {
            self.get(key).map(QueuedThread::entity_snapshot)
        }
    }

    /// Linux `requeue_task_rt(..., head = 1)` for an already linked wakee.
    pub(super) fn requeue_head(&mut self, key: RealtimeQueueKey) -> bool {
        let index = key.index();
        let Some(node) = self.node(key) else {
            return false;
        };
        if node.prev.is_none() {
            return true;
        }
        let node = self.active[index]
            .remove(key.node)
            .expect("RT key must remain linked under the rq lock");
        let requeued = self.active[index].push_front(node);
        debug_assert_eq!(requeued, key.node);
        true
    }

    /// Linux `task_tick_rt()` for one linked RR current.
    ///
    /// The current task stays in the active priority array.  Expiration
    /// refreshes its quantum unconditionally; only a peer at the same
    /// priority causes `requeue_task_rt()` and a reschedule request.
    pub(super) fn task_tick_round_robin(
        &mut self,
        key: RealtimeQueueKey,
        policy: SchedulePolicy,
    ) -> Option<bool> {
        let index = key.index();
        let expired = self.get(key)?.active.entity().round_robin_quantum_expired();
        if !expired {
            return Some(false);
        }

        let has_peer = self.active[index].len > 1;
        if has_peer {
            let mut node = self.active[index].remove(key.node)?;
            node.thread_mut()
                .active
                .entity_mut()
                .reset_round_robin_quantum(policy);
            let requeued = self.active[index].push_back(node);
            debug_assert_eq!(requeued, key.node);
        } else {
            self.get_mut(key)?
                .active
                .entity_mut()
                .reset_round_robin_quantum(policy);
        }
        Some(has_peer)
    }

    fn after_remove(&mut self, index: usize, mut node: Box<RealtimeNode>) -> QueuedThread {
        assert!(
            node.pushable.is_none(),
            "RT active node must leave its pushable list before dequeue"
        );
        debug_assert!(node.prev.is_none());
        debug_assert!(node.next.is_none());
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

    fn node(&self, key: RealtimeQueueKey) -> Option<&RealtimeNode> {
        let node = unsafe {
            // SAFETY: keys are created only after linking task-owned storage
            // and remain in the rq membership table until dequeue completes.
            key.node.as_ref()
        };
        (node.thread().id == key.thread).then_some(node)
    }

    fn node_mut(&mut self, mut key: RealtimeQueueKey) -> Option<&mut RealtimeNode> {
        let node = unsafe {
            // SAFETY: the owner rq lock and `&mut self` provide exclusive
            // access while this key remains published in rq membership.
            key.node.as_mut()
        };
        (node.thread().id == key.thread).then_some(node)
    }
}

fn bitmap_highest_priority(bitmap: u128) -> Option<u8> {
    (bitmap != 0).then(|| (u128::BITS - bitmap.leading_zeros()) as u8)
}
