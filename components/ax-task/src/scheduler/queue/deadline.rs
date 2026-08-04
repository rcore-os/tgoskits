//! Deadline-class runqueue ordered by active absolute deadline.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::cmp::Ordering;

use super::QueuedThread;
use crate::{SchedulingEntity, ThreadId};

/// Stable linkage for one entity in the Deadline runqueue.
///
/// The key is copied into the top-level membership table. This mirrors Linux's
/// embedded `rb_node`: dequeue and policy updates reach the linked entity
/// directly instead of rediscovering it by scanning the runnable set.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct DeadlineQueueKey {
    absolute_deadline_ns: u64,
    sequence: u64,
    thread: ThreadId,
}

impl DeadlineQueueKey {
    fn for_thread(thread: &QueuedThread) -> Self {
        Self {
            absolute_deadline_ns: deadline_entity(thread).absolute_deadline_ns(),
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
    min_event_ns: Option<u64>,
}

impl DeadlineNode {
    pub(crate) fn empty() -> Box<Self> {
        Box::new(Self {
            key: DeadlineQueueKey {
                absolute_deadline_ns: 0,
                sequence: 0,
                thread: ThreadId::from_parts(0, 0),
            },
            thread: None,
            left: None,
            right: None,
            height: 1,
            min_event_ns: None,
        })
    }

    fn reset(&mut self, thread: QueuedThread) {
        self.key = DeadlineQueueKey::for_thread(&thread);
        self.min_event_ns = scheduler_event(&thread);
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
        self.min_event_ns = earliest(
            scheduler_event(self.thread()),
            earliest(link_min_event(&self.left), link_min_event(&self.right)),
        );
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
    len: usize,
}

impl DeadlineRunQueue {
    pub(super) const fn new() -> Self {
        Self {
            root: None,
            keys: Vec::new(),
            len: 0,
        }
    }

    pub(super) fn prepare_thread_slot(&mut self, slot: usize) {
        if self.keys.len() <= slot {
            self.keys.resize(slot.saturating_add(1), None);
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
        thread.entity = entity;
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

    pub(super) fn pick_first(&mut self) -> Option<QueuedThread> {
        #[cfg(test)]
        super::record_deadline_runqueue_visit();
        let key = self.first().map(DeadlineQueueKey::for_thread)?;
        self.remove(key)
    }

    pub(super) fn find_first_matching(
        &self,
        predicate: &mut impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThread> {
        find_first_matching(self.root.as_deref(), predicate).cloned()
    }

    pub(super) fn earliest_event_ns(&self) -> Option<u64> {
        link_min_event(&self.root)
    }

    fn return_removed(mut removed: Box<DeadlineNode>) -> QueuedThread {
        let thread = removed
            .thread
            .take()
            .expect("removed Deadline node must retain its scheduling entity");
        removed.left = None;
        removed.right = None;
        removed.height = 1;
        removed.min_event_ns = None;
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
        assert_eq!(summary.min_event_ns, self.earliest_event_ns());
        assert_eq!(
            self.keys.iter().filter(|entry| entry.is_some()).count(),
            self.len
        );
    }
}

fn deadline_entity(thread: &QueuedThread) -> crate::DeadlineEntity {
    thread
        .entity
        .deadline()
        .expect("DeadlineRunQueue accepts only Deadline scheduling entities")
}

fn scheduler_event(thread: &QueuedThread) -> Option<u64> {
    let event = deadline_entity(thread).next_scheduler_event_ns();
    (event != 0).then_some(event)
}

fn earliest(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn link_height(link: &DeadlineLink) -> usize {
    link.as_deref().map_or(0, |node| node.height)
}

fn link_min_event(link: &DeadlineLink) -> Option<u64> {
    link.as_deref().and_then(|node| node.min_event_ns)
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

fn find_first_matching<'queue>(
    node: Option<&'queue DeadlineNode>,
    predicate: &mut impl FnMut(&QueuedThread) -> bool,
) -> Option<&'queue QueuedThread> {
    let node = node?;
    find_first_matching(node.left.as_deref(), predicate)
        .or_else(|| predicate(node.thread()).then_some(node.thread()))
        .or_else(|| find_first_matching(node.right.as_deref(), predicate))
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct ValidationSummary {
    count: usize,
    height: usize,
    min_event_ns: Option<u64>,
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
            min_event_ns: None,
        };
    };
    let left = validate_node(node.left.as_deref(), previous);
    assert!(previous.is_none_or(|key| key < node.key));
    *previous = Some(node.key);
    let right = validate_node(node.right.as_deref(), previous);
    let height = left.height.max(right.height).saturating_add(1);
    let min_event_ns = earliest(
        scheduler_event(node.thread()),
        earliest(left.min_event_ns, right.min_event_ns),
    );
    assert_eq!(node.height, height);
    assert_eq!(node.min_event_ns, min_event_ns);
    assert!(left.height.abs_diff(right.height) <= 1);
    ValidationSummary {
        count: left.count.saturating_add(right.count).saturating_add(1),
        height,
        min_event_ns,
    }
}
