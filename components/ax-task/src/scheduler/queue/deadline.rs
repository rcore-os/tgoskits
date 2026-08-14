//! Deadline-class runqueue ordered by active absolute deadline.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::cmp::Ordering;

use super::{QueuedThread, QueuedThreadSnapshot, deadline_pushable::DeadlinePushableTasks};
use crate::{
    DeadlineBandwidthSnapshot, SchedulingEntity, TaskError, ThreadCore, ThreadId,
    runtime::task_runtime,
};

/// Linux `dl_rq` bandwidth ledger. It lives beside the active EDF tree,
/// throttled entities, and timer lifetime anchors under the same rq lock.
#[derive(Debug)]
struct DeadlineRunQueueBandwidth {
    this_bw_scaled: u64,
    running_bw_scaled: u64,
    max_bw_scaled: u64,
}

impl DeadlineRunQueueBandwidth {
    const fn new(max_bw_scaled: u64) -> Self {
        Self {
            this_bw_scaled: 0,
            running_bw_scaled: 0,
            max_bw_scaled,
        }
    }

    fn add(&mut self, utilization_scaled: u64, active: bool) {
        let this_bw_scaled = self
            .this_bw_scaled
            .checked_add(utilization_scaled)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x444c_1001, utilization_scaled as usize)
            });
        let running_bw_scaled = if active {
            self.running_bw_scaled
                .checked_add(utilization_scaled)
                .unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x444c_1002, utilization_scaled as usize)
                })
        } else {
            self.running_bw_scaled
        };
        if running_bw_scaled > this_bw_scaled {
            task_runtime::fatal_invariant(0x444c_1003, utilization_scaled as usize);
        }
        self.this_bw_scaled = this_bw_scaled;
        self.running_bw_scaled = running_bw_scaled;
    }

    fn remove(&mut self, utilization_scaled: u64, active: bool) {
        let this_bw_scaled = self
            .this_bw_scaled
            .checked_sub(utilization_scaled)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x444c_1004, utilization_scaled as usize)
            });
        let running_bw_scaled = if active {
            self.running_bw_scaled
                .checked_sub(utilization_scaled)
                .unwrap_or_else(|| {
                    task_runtime::fatal_invariant(0x444c_1005, utilization_scaled as usize)
                })
        } else {
            self.running_bw_scaled
        };
        if running_bw_scaled > this_bw_scaled {
            task_runtime::fatal_invariant(0x444c_1006, utilization_scaled as usize);
        }
        self.this_bw_scaled = this_bw_scaled;
        self.running_bw_scaled = running_bw_scaled;
    }

    fn activate(&mut self, utilization_scaled: u64) {
        let running_bw_scaled = self
            .running_bw_scaled
            .checked_add(utilization_scaled)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x444c_1007, utilization_scaled as usize)
            });
        if running_bw_scaled > self.this_bw_scaled {
            task_runtime::fatal_invariant(0x444c_1008, utilization_scaled as usize);
        }
        self.running_bw_scaled = running_bw_scaled;
    }

    fn deactivate(&mut self, utilization_scaled: u64) {
        self.running_bw_scaled = self
            .running_bw_scaled
            .checked_sub(utilization_scaled)
            .unwrap_or_else(|| {
                task_runtime::fatal_invariant(0x444c_1009, utilization_scaled as usize)
            });
    }

    const fn snapshot(&self) -> DeadlineBandwidthSnapshot {
        DeadlineBandwidthSnapshot::new(
            self.this_bw_scaled,
            self.running_bw_scaled,
            self.max_bw_scaled,
        )
    }
}

/// Stable linkage for one entity in the Deadline runqueue.
///
/// The key is copied into the top-level membership table. This mirrors Linux's
/// embedded `rb_node`: dequeue and policy updates reach the linked entity
/// directly instead of rediscovering it by scanning the runnable set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DeadlineQueueKey {
    absolute_deadline_ns: u64,
    sequence: u64,
    thread: ThreadId,
}

impl Ord for DeadlineQueueKey {
    fn cmp(&self, other: &Self) -> Ordering {
        crate::scheduler_time_cmp(self.absolute_deadline_ns, other.absolute_deadline_ns)
            .then_with(|| self.sequence.cmp(&other.sequence))
            .then_with(|| self.thread.cmp(&other.thread))
    }
}

impl PartialOrd for DeadlineQueueKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl DeadlineQueueKey {
    pub(super) const fn empty() -> Self {
        Self {
            absolute_deadline_ns: 0,
            sequence: 0,
            thread: ThreadId::from_parts(0, 0),
        }
    }

    pub(super) const fn thread(self) -> ThreadId {
        self.thread
    }

    fn for_thread(thread: &QueuedThread) -> Self {
        Self {
            absolute_deadline_ns: deadline_entity(thread)
                .absolute_deadline_ns()
                .expect("a queued Deadline entity must own an absolute deadline"),
            sequence: thread.sequence,
            thread: thread.id,
        }
    }
}

type DeadlineLink = Option<Box<DeadlineNode>>;

#[derive(Debug)]
pub(crate) struct DeadlineNode {
    key: DeadlineQueueKey,
    thread: Option<QueuedThread>,
    left: DeadlineLink,
    right: DeadlineLink,
    height: usize,
}

impl DeadlineNode {
    pub(crate) fn empty() -> Box<Self> {
        Box::new(Self {
            key: DeadlineQueueKey::empty(),
            thread: None,
            left: None,
            right: None,
            height: 1,
        })
    }

    fn reset(&mut self, thread: QueuedThread) {
        self.key = DeadlineQueueKey::for_thread(&thread);
        self.thread = Some(thread);
        self.left = None;
        self.right = None;
        self.height = 1;
    }

    fn thread(&self) -> &QueuedThread {
        self.thread
            .as_ref()
            .expect("linked Deadline node must own one scheduling entity")
    }

    fn refresh(&mut self) {
        self.height = link_height(&self.left)
            .max(link_height(&self.right))
            .saturating_add(1);
    }
}

/// State owned exclusively by the Deadline scheduling class of one CPU.
///
/// The AVL tree is the Rust equivalent of Linux's cached Deadline rb-tree. A
/// removed node is retained on an owner-local free list, so repeated
/// dequeue/enqueue scheduling cycles neither allocate nor free memory.
#[derive(Debug)]
pub(super) struct DeadlineRunQueue {
    root: DeadlineLink,
    keys: Vec<Option<(u32, DeadlineQueueKey)>>,
    throttled: Vec<Option<(u32, QueuedThread)>>,
    members: Vec<Arc<ThreadCore>>,
    bandwidth: DeadlineRunQueueBandwidth,
    pushable: DeadlinePushableTasks,
    len: usize,
}

impl DeadlineRunQueue {
    pub(super) fn new(max_bw_scaled: u64, thread_capacity: usize) -> Self {
        Self {
            root: None,
            keys: Vec::new(),
            throttled: Vec::new(),
            members: Vec::with_capacity(thread_capacity),
            bandwidth: DeadlineRunQueueBandwidth::new(max_bw_scaled),
            pushable: DeadlinePushableTasks::new(),
            len: 0,
        }
    }

    pub(super) fn prepare_thread_slot(&mut self, slot: usize) {
        if self.keys.len() <= slot {
            self.keys.resize(slot.saturating_add(1), None);
        }
        if self.throttled.len() <= slot {
            self.throttled.resize_with(slot.saturating_add(1), || None);
        }
        self.pushable.prepare_thread_slot(slot);
    }

    pub(super) fn install_throttled(&mut self, thread: QueuedThread) -> Result<(), TaskError> {
        let entry = self
            .throttled
            .get_mut(thread.id.slot() as usize)
            .ok_or(TaskError::InvalidConfiguration)?;
        if entry.is_some() {
            return Err(TaskError::AlreadyQueued);
        }
        *entry = Some((thread.id.generation(), thread));
        Ok(())
    }

    pub(super) fn throttled(&self, id: ThreadId) -> Option<&QueuedThread> {
        let (generation, thread) = self.throttled.get(id.slot() as usize)?.as_ref()?;
        (*generation == id.generation()).then_some(thread)
    }

    pub(super) fn throttled_mut(&mut self, id: ThreadId) -> Option<&mut QueuedThread> {
        let (generation, thread) = self.throttled.get_mut(id.slot() as usize)?.as_mut()?;
        (*generation == id.generation()).then_some(thread)
    }

    pub(super) fn take_throttled(&mut self, id: ThreadId) -> Option<QueuedThread> {
        let entry = self.throttled.get_mut(id.slot() as usize)?;
        if entry
            .as_ref()
            .is_none_or(|(generation, _)| *generation != id.generation())
        {
            return None;
        }
        entry.take().map(|(_, thread)| thread)
    }

    pub(super) fn members_are_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub(super) fn member(&self, thread: ThreadId) -> Option<Arc<ThreadCore>> {
        self.members
            .iter()
            .find(|member| member.id() == thread)
            .map(Arc::clone)
    }

    pub(super) fn register_member(&mut self, core: &Arc<ThreadCore>) -> bool {
        if self.members.iter().any(|member| Arc::ptr_eq(member, core)) {
            return false;
        }
        assert!(
            self.members.len() < self.members.capacity(),
            "thread construction must reserve every Deadline member slot"
        );
        self.members.push(Arc::clone(core));
        true
    }

    pub(super) fn unregister_member(&mut self, core: &Arc<ThreadCore>) {
        if let Some(index) = self
            .members
            .iter()
            .position(|member| Arc::ptr_eq(member, core))
        {
            self.members.swap_remove(index);
        }
    }

    pub(super) fn add_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.bandwidth.add(utilization_scaled, active);
    }

    pub(super) fn remove_bandwidth(&mut self, utilization_scaled: u64, active: bool) {
        self.bandwidth.remove(utilization_scaled, active);
    }

    pub(super) fn activate_bandwidth(&mut self, utilization_scaled: u64) {
        self.bandwidth.activate(utilization_scaled);
    }

    pub(super) fn deactivate_bandwidth(&mut self, utilization_scaled: u64) {
        self.bandwidth.deactivate(utilization_scaled);
    }

    pub(super) const fn bandwidth(&self) -> DeadlineBandwidthSnapshot {
        self.bandwidth.snapshot()
    }

    pub(super) const fn has_pushable(&self) -> bool {
        !self.pushable.is_empty()
    }

    pub(super) const fn pushable_count(&self) -> usize {
        self.pushable.len()
    }

    pub(super) fn refresh_pushable(&mut self, thread: ThreadId, current: Option<ThreadId>) {
        let queued = self
            .keys
            .get(thread.slot() as usize)
            .and_then(|entry| *entry)
            .filter(|(generation, _)| *generation == thread.generation())
            .and_then(|(_, key)| {
                self.get(key).map(|entry| {
                    (
                        key,
                        entry.migration_capable && current != Some(thread),
                        Arc::clone(&entry.core),
                    )
                })
            });
        let Some((key, should_be_pushable, core)) = queued else {
            return;
        };
        let is_pushable = self.pushable.contains(thread);
        match (is_pushable, should_be_pushable) {
            (false, true) => self.pushable.insert(key, &core),
            (true, false) => {
                let removed = self.pushable.remove(key, &core);
                debug_assert!(removed);
            }
            _ => {}
        }
    }

    pub(super) fn insert(&mut self, thread: QueuedThread) -> DeadlineQueueKey {
        let key = DeadlineQueueKey::for_thread(&thread);
        let slot = thread.id.slot() as usize;
        assert!(
            self.keys.len() > slot,
            "thread construction must prepare Deadline rq membership"
        );
        assert!(
            self.keys[slot]
                .replace((thread.id.generation(), key))
                .is_none(),
            "Deadline runqueue cannot contain one thread twice"
        );
        let core = Arc::clone(&thread.core);
        let mut inserted = unsafe {
            // SAFETY: target-rq placement serializes the only physical queue
            // node belonging to this thread. A linked thread cannot enter a
            // second runqueue concurrently.
            core.runqueue_nodes().take_deadline()
        };
        inserted.reset(thread);
        self.root = insert_node(self.root.take(), inserted);
        self.len = self.len.saturating_add(1);
        key
    }

    pub(super) fn remove(&mut self, key: DeadlineQueueKey) -> Option<QueuedThread> {
        let indexed = self.keys.get_mut(key.thread.slot() as usize)?.take()?;
        if indexed != (key.thread.generation(), key) {
            self.keys[key.thread.slot() as usize] = Some(indexed);
            return None;
        }
        if self.pushable.contains(key.thread) {
            let core = Arc::clone(&self.get(key)?.core);
            assert!(self.pushable.remove(key, &core));
        }
        let (root, removed) = remove_node(self.root.take(), key);
        self.root = root;
        let removed = removed.expect("Deadline identity index must match its ordered tree");
        self.len -= 1;
        Some(Self::return_removed(removed))
    }

    pub(super) fn update_entity(
        &mut self,
        key: DeadlineQueueKey,
        entity: SchedulingEntity,
    ) -> Option<DeadlineQueueKey> {
        let mut thread = self.remove(key)?;
        *thread.active.entity_mut() = entity;
        Some(self.insert(thread))
    }

    pub(super) fn first(&self) -> Option<&QueuedThread> {
        let mut node = self.root.as_deref()?;
        while let Some(left) = node.left.as_deref() {
            node = left;
        }
        Some(node.thread())
    }

    pub(super) fn get(&self, key: DeadlineQueueKey) -> Option<&QueuedThread> {
        find_node(self.root.as_deref(), key).map(DeadlineNode::thread)
    }

    pub(super) fn get_mut(&mut self, key: DeadlineQueueKey) -> Option<&mut QueuedThread> {
        find_node_mut(self.root.as_deref_mut(), key).and_then(|node| node.thread.as_mut())
    }

    pub(super) fn select_first(&self) -> Option<QueuedThreadSnapshot> {
        #[cfg(test)]
        super::record_deadline_runqueue_visit();
        self.first().map(QueuedThreadSnapshot::from)
    }

    pub(super) fn put_prev_current(
        &mut self,
        key: DeadlineQueueKey,
    ) -> Option<(DeadlineQueueKey, SchedulingEntity)> {
        let thread = self.remove(key)?;
        let entity = thread.entity_snapshot();
        let new_key = self.insert(thread);
        Some((new_key, entity))
    }

    pub(super) fn find_first_pushable_matching(
        &self,
        predicate: &mut impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThreadSnapshot> {
        self.pushable
            .find_first_matching(|key| self.get(key).is_some_and(&mut *predicate))
            .and_then(|key| self.get(key))
            .map(QueuedThreadSnapshot::from)
    }

    pub(super) fn earliest_deadline_ns(&self) -> Option<u64> {
        self.first()
            .and_then(|thread| deadline_entity(thread).absolute_deadline_ns())
    }

    fn return_removed(mut removed: Box<DeadlineNode>) -> QueuedThread {
        let thread = removed
            .thread
            .take()
            .expect("removed Deadline node must retain its scheduling entity");
        removed.left = None;
        removed.right = None;
        removed.height = 1;
        unsafe {
            // SAFETY: removal has unlinked this node from the sole owner rq;
            // placement cannot re-enqueue until the caller completes the rq
            // transaction.
            thread.core.runqueue_nodes().return_deadline(removed);
        }
        thread
    }

    #[cfg(test)]
    pub(super) fn assert_invariants(&self) {
        let mut previous = None;
        let summary = validate_node(self.root.as_deref(), &mut previous);
        assert_eq!(summary.count, self.len);
        assert_eq!(
            self.keys.iter().filter(|entry| entry.is_some()).count(),
            self.len
        );
        self.pushable.assert_invariants();
    }
}

fn deadline_entity(thread: &QueuedThread) -> &crate::DeadlineEntity {
    thread
        .active
        .entity()
        .deadline()
        .expect("DeadlineRunQueue accepts only Deadline scheduling entities")
}

fn link_height(link: &DeadlineLink) -> usize {
    link.as_deref().map_or(0, |node| node.height)
}

fn balance_factor(node: &DeadlineNode) -> isize {
    link_height(&node.left) as isize - link_height(&node.right) as isize
}

fn rotate_left(mut root: Box<DeadlineNode>) -> Box<DeadlineNode> {
    let mut promoted = root
        .right
        .take()
        .expect("left rotation requires a right child");
    root.right = promoted.left.take();
    root.refresh();
    promoted.left = Some(root);
    promoted.refresh();
    promoted
}

fn rotate_right(mut root: Box<DeadlineNode>) -> Box<DeadlineNode> {
    let mut promoted = root
        .left
        .take()
        .expect("right rotation requires a left child");
    root.left = promoted.right.take();
    root.refresh();
    promoted.right = Some(root);
    promoted.refresh();
    promoted
}

fn rebalance(mut node: Box<DeadlineNode>) -> Box<DeadlineNode> {
    node.refresh();
    match balance_factor(&node) {
        factor if factor > 1 => {
            if node
                .left
                .as_deref()
                .is_some_and(|left| balance_factor(left) < 0)
            {
                let left = node
                    .left
                    .take()
                    .expect("balance factor requires a left child");
                node.left = Some(rotate_left(left));
            }
            rotate_right(node)
        }
        factor if factor < -1 => {
            if node
                .right
                .as_deref()
                .is_some_and(|right| balance_factor(right) > 0)
            {
                let right = node
                    .right
                    .take()
                    .expect("balance factor requires a right child");
                node.right = Some(rotate_right(right));
            }
            rotate_left(node)
        }
        _ => node,
    }
}

fn insert_node(root: DeadlineLink, inserted: Box<DeadlineNode>) -> DeadlineLink {
    let Some(mut root) = root else {
        return Some(inserted);
    };
    match inserted.key.cmp(&root.key) {
        Ordering::Less => root.left = insert_node(root.left.take(), inserted),
        Ordering::Greater => root.right = insert_node(root.right.take(), inserted),
        Ordering::Equal => panic!("Deadline runqueue key must be unique"),
    }
    Some(rebalance(root))
}

fn remove_node(
    root: DeadlineLink,
    key: DeadlineQueueKey,
) -> (DeadlineLink, Option<Box<DeadlineNode>>) {
    let Some(mut root) = root else {
        return (None, None);
    };
    match key.cmp(&root.key) {
        Ordering::Less => {
            let (left, removed) = remove_node(root.left.take(), key);
            root.left = left;
            (Some(rebalance(root)), removed)
        }
        Ordering::Greater => {
            let (right, removed) = remove_node(root.right.take(), key);
            root.right = right;
            (Some(rebalance(root)), removed)
        }
        Ordering::Equal => match (root.left.take(), root.right.take()) {
            (None, right) => (right, Some(root)),
            (left, None) => (left, Some(root)),
            (Some(left), Some(right)) => {
                let (right, mut successor) = take_min(right);
                successor.left = Some(left);
                successor.right = right;
                (Some(rebalance(successor)), Some(root))
            }
        },
    }
}

fn take_min(mut root: Box<DeadlineNode>) -> (DeadlineLink, Box<DeadlineNode>) {
    let Some(left) = root.left.take() else {
        let right = root.right.take();
        root.refresh();
        return (right, root);
    };
    let (left, minimum) = take_min(left);
    root.left = left;
    (Some(rebalance(root)), minimum)
}

fn find_node(node: Option<&DeadlineNode>, key: DeadlineQueueKey) -> Option<&DeadlineNode> {
    let node = node?;
    match key.cmp(&node.key) {
        Ordering::Less => find_node(node.left.as_deref(), key),
        Ordering::Greater => find_node(node.right.as_deref(), key),
        Ordering::Equal => Some(node),
    }
}

fn find_node_mut(
    node: Option<&mut DeadlineNode>,
    key: DeadlineQueueKey,
) -> Option<&mut DeadlineNode> {
    let node = node?;
    match key.cmp(&node.key) {
        Ordering::Less => find_node_mut(node.left.as_deref_mut(), key),
        Ordering::Greater => find_node_mut(node.right.as_deref_mut(), key),
        Ordering::Equal => Some(node),
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct ValidationSummary {
    count: usize,
    height: usize,
}

#[cfg(test)]
fn validate_node(
    node: Option<&DeadlineNode>,
    previous: &mut Option<DeadlineQueueKey>,
) -> ValidationSummary {
    let Some(node) = node else {
        return ValidationSummary {
            count: 0,
            height: 0,
        };
    };
    let left = validate_node(node.left.as_deref(), previous);
    assert!(previous.is_none_or(|key| key < node.key));
    *previous = Some(node.key);
    let right = validate_node(node.right.as_deref(), previous);
    let height = left.height.max(right.height).saturating_add(1);
    assert_eq!(node.height, height);
    assert!(left.height.abs_diff(right.height) <= 1);
    ValidationSummary {
        count: left.count.saturating_add(right.count).saturating_add(1),
        height,
    }
}
