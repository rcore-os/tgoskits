//! Deadline-class pushable tasks ordered independently from the active EDF tree.
//!
//! Linux keeps `dl_rq::pushable_dl_tasks_root` as a second rb-tree whose nodes
//! are embedded in each task.  This AVL tree provides the same ownership and
//! ordering contract without allocating while the owner rq lock is held.

use alloc::{boxed::Box, vec::Vec};
use core::cmp::Ordering;

use super::deadline::DeadlineQueueKey;
use crate::{ThreadCore, ThreadId};

type DeadlinePushableLink = Option<Box<DeadlinePushableNode>>;

/// Per-thread linkage for `DeadlinePushableTasks`.
#[derive(Debug)]
pub(crate) struct DeadlinePushableNode {
    key: DeadlineQueueKey,
    left: DeadlinePushableLink,
    right: DeadlinePushableLink,
    height: usize,
}

impl DeadlinePushableNode {
    pub(crate) fn empty() -> Box<Self> {
        Box::new(Self {
            key: DeadlineQueueKey::empty(),
            left: None,
            right: None,
            height: 1,
        })
    }

    fn reset(&mut self, key: DeadlineQueueKey) {
        self.key = key;
        self.left = None;
        self.right = None;
        self.height = 1;
    }

    fn refresh(&mut self) {
        self.height = link_height(&self.left)
            .max(link_height(&self.right))
            .saturating_add(1);
    }
}

/// Linux `dl_rq::pushable_dl_tasks_root` equivalent.
#[derive(Debug)]
pub(super) struct DeadlinePushableTasks {
    root: DeadlinePushableLink,
    keys: Vec<Option<(u32, DeadlineQueueKey)>>,
    len: usize,
}

impl DeadlinePushableTasks {
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

    pub(super) const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(super) const fn len(&self) -> usize {
        self.len
    }

    pub(super) fn contains(&self, thread: ThreadId) -> bool {
        self.keys
            .get(thread.slot() as usize)
            .and_then(|entry| *entry)
            .is_some_and(|(generation, _)| generation == thread.generation())
    }

    pub(super) fn insert(&mut self, key: DeadlineQueueKey, core: &ThreadCore) {
        let slot = key.thread().slot() as usize;
        let index = self
            .keys
            .get_mut(slot)
            .expect("thread construction must prepare Deadline pushable membership");
        assert!(
            index.replace((key.thread().generation(), key)).is_none(),
            "one Deadline task cannot own two pushable links"
        );
        let mut node = unsafe {
            // SAFETY: the task is linked to this owner rq and the rq lock
            // serializes its independent pushable-tree membership.
            core.runqueue_nodes().take_deadline_pushable()
        };
        node.reset(key);
        self.root = insert_node(self.root.take(), node);
        self.len = self
            .len
            .checked_add(1)
            .expect("Deadline pushable count must fit usize");
    }

    pub(super) fn remove(&mut self, key: DeadlineQueueKey, core: &ThreadCore) -> bool {
        let Some(index) = self.keys.get_mut(key.thread().slot() as usize) else {
            return false;
        };
        if *index != Some((key.thread().generation(), key)) {
            return false;
        }
        *index = None;
        let (root, removed) = remove_node(self.root.take(), key);
        self.root = root;
        let mut removed = removed.expect("Deadline pushable index must match its ordered tree");
        removed.left = None;
        removed.right = None;
        removed.height = 1;
        unsafe {
            // SAFETY: removal detached the node from the only pushable tree
            // before returning it to task-owned storage.
            core.runqueue_nodes().return_deadline_pushable(removed);
        }
        self.len = self
            .len
            .checked_sub(1)
            .expect("Deadline pushable count must match membership");
        true
    }

    pub(super) fn find_first_matching(
        &self,
        mut predicate: impl FnMut(DeadlineQueueKey) -> bool,
    ) -> Option<DeadlineQueueKey> {
        find_first_matching(self.root.as_deref(), &mut predicate)
    }
}

fn link_height(link: &DeadlinePushableLink) -> usize {
    link.as_deref().map_or(0, |node| node.height)
}

fn balance_factor(node: &DeadlinePushableNode) -> isize {
    link_height(&node.left) as isize - link_height(&node.right) as isize
}

fn rotate_left(mut root: Box<DeadlinePushableNode>) -> Box<DeadlinePushableNode> {
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

fn rotate_right(mut root: Box<DeadlinePushableNode>) -> Box<DeadlinePushableNode> {
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

fn rebalance(mut node: Box<DeadlinePushableNode>) -> Box<DeadlinePushableNode> {
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

fn insert_node(
    root: DeadlinePushableLink,
    inserted: Box<DeadlinePushableNode>,
) -> DeadlinePushableLink {
    let Some(mut root) = root else {
        return Some(inserted);
    };
    match inserted.key.cmp(&root.key) {
        Ordering::Less => root.left = insert_node(root.left.take(), inserted),
        Ordering::Greater => root.right = insert_node(root.right.take(), inserted),
        Ordering::Equal => panic!("Deadline pushable key must be unique"),
    }
    Some(rebalance(root))
}

fn remove_node(
    root: DeadlinePushableLink,
    key: DeadlineQueueKey,
) -> (DeadlinePushableLink, Option<Box<DeadlinePushableNode>>) {
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

fn take_min(
    mut root: Box<DeadlinePushableNode>,
) -> (DeadlinePushableLink, Box<DeadlinePushableNode>) {
    let Some(left) = root.left.take() else {
        let right = root.right.take();
        root.refresh();
        return (right, root);
    };
    let (left, minimum) = take_min(left);
    root.left = left;
    (Some(rebalance(root)), minimum)
}

fn find_first_matching(
    node: Option<&DeadlinePushableNode>,
    predicate: &mut impl FnMut(DeadlineQueueKey) -> bool,
) -> Option<DeadlineQueueKey> {
    let node = node?;
    find_first_matching(node.left.as_deref(), predicate)
        .or_else(|| predicate(node.key).then_some(node.key))
        .or_else(|| find_first_matching(node.right.as_deref(), predicate))
}
