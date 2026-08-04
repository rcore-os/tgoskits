//! Fixed-priority FIFO/RR runqueue owned by the real-time scheduling class.

use alloc::{boxed::Box, sync::Arc};
use core::ptr::NonNull;

use super::{EnqueueReason, QueuedThread};
use crate::ThreadId;

const RT_PRIORITY_LEVELS: usize = 99;

/// Per-thread RT linkage prepared during thread construction.
#[derive(Debug)]
pub(crate) struct RealtimeNode {
    thread: Option<QueuedThread>,
    next: Option<Box<RealtimeNode>>,
}

impl RealtimeNode {
    pub(crate) fn empty() -> Box<Self> {
        Box::new(Self {
            thread: None,
            next: None,
        })
    }

    fn reset(&mut self, thread: QueuedThread) {
        self.thread = Some(thread);
        self.next = None;
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

    fn pop_front(&mut self) -> Option<Box<RealtimeNode>> {
        let mut removed = self.head.take()?;
        self.head = removed.next.take();
        self.len -= 1;
        if self.head.is_none() {
            self.tail = None;
        }
        Some(removed)
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

    fn insert_at(&mut self, position: usize, mut node: Box<RealtimeNode>) {
        if position == 0 {
            self.push_front(node);
            return;
        }
        if position >= self.len {
            self.push_back(node);
            return;
        }
        let mut link = &mut self.head;
        for _ in 0..position {
            link = &mut link
                .as_mut()
                .expect("RT restore position must remain within the level")
                .next;
        }
        node.next = link.take();
        *link = Some(node);
        self.len += 1;
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

impl<'queue> Iterator for RealtimeIter<'queue> {
    type Item = &'queue QueuedThread;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.next?;
        self.next = node.next.as_deref();
        Some(node.thread())
    }
}

/// Linux-style RT priority array: one intrusive FIFO per priority plus cached
/// bitmaps. Queue nodes are embedded scheduler storage; enqueue/dequeue never
/// allocate or free memory while the rq lock is held.
#[derive(Debug)]
pub(super) struct RealtimeRunQueue {
    active: [RealtimeLevel; RT_PRIORITY_LEVELS],
    active_bitmap: u128,
    exempt_bitmap: u128,
    exempt_count: [usize; RT_PRIORITY_LEVELS],
}

impl RealtimeRunQueue {
    pub(super) fn new() -> Self {
        Self {
            active: core::array::from_fn(|_| RealtimeLevel::new()),
            active_bitmap: 0,
            exempt_bitmap: 0,
            exempt_count: [0; RT_PRIORITY_LEVELS],
        }
    }

    pub(super) const fn has_any(&self) -> bool {
        self.active_bitmap != 0
    }

    pub(super) fn highest_priority(&self) -> Option<u8> {
        bitmap_highest_priority(self.active_bitmap)
    }

    pub(super) fn count_at_priority(&self, priority: u8) -> usize {
        priority
            .checked_sub(1)
            .and_then(|index| self.active.get(index as usize))
            .map_or(0, |level| level.len)
    }

    pub(super) fn enqueue(&mut self, thread: QueuedThread, reason: EnqueueReason) -> u8 {
        let priority = thread
            .policy
            .rt_priority()
            .expect("RT queue requires RT policy")
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
        let node = self.active[index].remove_at(position)?;
        Some(self.after_remove(index, node))
    }

    pub(super) fn detach(&mut self, priority: u8, id: ThreadId) -> Option<(QueuedThread, usize)> {
        let index = (priority - 1) as usize;
        let position = self.active[index].position(id)?;
        let node = self.active[index].remove_at(position)?;
        Some((self.after_remove(index, node), position))
    }

    pub(super) fn restore(&mut self, priority: u8, position: usize, thread: QueuedThread) {
        let index = (priority - 1) as usize;
        if thread.rt_quota_exempt {
            self.exempt_count[index] = self.exempt_count[index].saturating_add(1);
            self.exempt_bitmap |= 1_u128 << index;
        }
        let core = Arc::clone(&thread.core);
        let mut node = unsafe {
            // SAFETY: a detached transfer retains exclusive placement of this
            // thread until either restore or target publication commits.
            core.runqueue_nodes().take_realtime()
        };
        node.reset(thread);
        self.active[index].insert_at(position, node);
        self.active_bitmap |= 1_u128 << index;
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

    pub(super) fn first(&self) -> Option<&QueuedThread> {
        let priority = self.highest_priority()?;
        self.active[(priority - 1) as usize].iter().next()
    }

    pub(super) fn find_first_matching(
        &self,
        predicate: &mut impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThread> {
        self.active
            .iter()
            .rev()
            .find_map(|level| level.iter().find(|thread| predicate(thread)).cloned())
    }

    pub(super) fn pick(&mut self, ordinary_may_run: bool) -> Option<QueuedThread> {
        let priority = if ordinary_may_run {
            self.highest_priority()?
        } else {
            bitmap_highest_priority(self.exempt_bitmap)?
        };
        let index = (priority - 1) as usize;
        let node = if ordinary_may_run {
            self.active[index].pop_front()
        } else {
            let position = self.active[index]
                .iter()
                .position(|thread| thread.rt_quota_exempt)
                .expect("RT exempt bitmap must identify an exempt entry");
            self.active[index].remove_at(position)
        }
        .expect("RT bitmap must identify a non-empty priority queue");
        Some(self.after_remove(index, node))
    }

    fn after_remove(&mut self, index: usize, mut node: Box<RealtimeNode>) -> QueuedThread {
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
        }
        unsafe {
            // SAFETY: the node is no longer linked and placement prevents a
            // concurrent enqueue until this rq transaction returns it.
            thread.core.runqueue_nodes().return_realtime(node);
        }
        thread
    }
}

fn bitmap_highest_priority(bitmap: u128) -> Option<u8> {
    (bitmap != 0).then(|| (u128::BITS - bitmap.leading_zeros()) as u8)
}
