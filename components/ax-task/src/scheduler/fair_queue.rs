//! Owner-local EEVDF runqueue with incremental lag accounting.

use alloc::{boxed::Box, vec::Vec};
use core::cmp::Ordering;

use super::queue::QueuedThread;
#[cfg(test)]
use super::queue::record_fair_runqueue_visit;
use crate::{FairEntity, ThreadId};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FairQueueKey {
    virtual_deadline: u64,
    sequence: u64,
    thread: ThreadId,
}

impl FairQueueKey {
    fn for_thread(thread: &QueuedThread) -> Self {
        let fair = fair_entity(thread);
        Self {
            virtual_deadline: fair.virtual_deadline(),
            sequence: thread.sequence,
            thread: thread.id,
        }
    }
}

type FairLink = Option<Box<FairNode>>;

#[derive(Debug)]
struct FairNode {
    key: FairQueueKey,
    thread: Option<QueuedThread>,
    left: FairLink,
    right: FairLink,
    height: usize,
    min_vruntime: u64,
}

impl FairNode {
    fn new(thread: QueuedThread) -> Box<Self> {
        let key = FairQueueKey::for_thread(&thread);
        let min_vruntime = fair_entity(&thread).vruntime();
        Box::new(Self {
            key,
            thread: Some(thread),
            left: None,
            right: None,
            height: 1,
            min_vruntime,
        })
    }

    fn reset(&mut self, thread: QueuedThread) {
        self.key = FairQueueKey::for_thread(&thread);
        self.min_vruntime = fair_entity(&thread).vruntime();
        self.thread = Some(thread);
        self.left = None;
        self.right = None;
        self.height = 1;
    }

    fn thread(&self) -> &QueuedThread {
        self.thread
            .as_ref()
            .expect("linked fair node must own one scheduling entity")
    }

    fn refresh(&mut self) {
        self.height = link_height(&self.left)
            .max(link_height(&self.right))
            .saturating_add(1);
        self.min_vruntime = fair_entity(self.thread())
            .vruntime()
            .min(link_min_vruntime(&self.left))
            .min(link_min_vruntime(&self.right));
    }
}

/// A fair-class queue ordered by virtual deadline and augmented by minimum
/// vruntime. The augmentation makes earliest-eligible selection logarithmic.
#[derive(Debug)]
pub(super) struct FairRunQueue {
    root: FairLink,
    spare: FairLink,
    keys: Vec<Option<(u32, FairQueueKey)>>,
    weighted_vruntime: u128,
    total_weight: u128,
    len: usize,
}

impl FairRunQueue {
    pub(super) fn new() -> Self {
        Self {
            root: None,
            spare: None,
            keys: Vec::new(),
            weighted_vruntime: 0,
            total_weight: 0,
            len: 0,
        }
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(super) fn insert(&mut self, thread: QueuedThread) {
        let key = FairQueueKey::for_thread(&thread);
        let slot = thread.id.slot() as usize;
        if self.keys.len() <= slot {
            self.keys.resize(slot.saturating_add(1), None);
        }
        assert!(
            self.keys[slot]
                .replace((thread.id.generation(), key))
                .is_none(),
            "fair runqueue cannot contain one thread twice"
        );
        self.add_weighted_entity(fair_entity(&thread));
        let inserted = if let Some(mut spare) = self.spare.take() {
            self.spare = spare.right.take();
            spare.reset(thread);
            spare
        } else {
            FairNode::new(thread)
        };
        self.root = insert_node(self.root.take(), inserted);
        self.len = self.len.saturating_add(1);
    }

    pub(super) fn remove(&mut self, thread: ThreadId) -> Option<QueuedThread> {
        let indexed = self.keys.get_mut(thread.slot() as usize)?.take()?;
        if indexed.0 != thread.generation() {
            self.keys[thread.slot() as usize] = Some(indexed);
            return None;
        }
        let key = indexed.1;
        let (root, removed) = remove_node(self.root.take(), key);
        self.root = root;
        let removed = removed.expect("fair runqueue identity index must match its tree");
        let removed = self.recycle_removed(removed);
        self.remove_weighted_entity(fair_entity(&removed));
        self.len -= 1;
        Some(removed)
    }

    pub(super) fn pick_eligible(&mut self, virtual_time: u64) -> Option<QueuedThread> {
        let key = earliest_eligible_key(self.root.as_deref(), virtual_time)?;
        let indexed = self.keys[key.thread.slot() as usize]
            .take()
            .expect("eligible fair node must remain indexed");
        assert_eq!(indexed, (key.thread.generation(), key));
        let (root, removed) = remove_node(self.root.take(), key);
        self.root = root;
        let removed = removed.expect("eligible fair key must remain present until owner removal");
        let removed = self.recycle_removed(removed);
        self.remove_weighted_entity(fair_entity(&removed));
        self.len -= 1;
        Some(removed)
    }

    pub(super) fn first(&self) -> Option<&QueuedThread> {
        let mut node = self.root.as_deref()?;
        while let Some(left) = node.left.as_deref() {
            node = left;
        }
        Some(node.thread())
    }

    pub(super) fn find_first_matching(
        &self,
        predicate: &mut impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThread> {
        find_first_matching(self.root.as_deref(), predicate).cloned()
    }

    pub(super) fn weighted_virtual_time(&self, current: Option<FairEntity>) -> Option<u64> {
        let mut weighted_vruntime = self.weighted_vruntime;
        let mut total_weight = self.total_weight;
        if let Some(current) = current {
            let weight = u128::from(current.weight());
            weighted_vruntime = weighted_vruntime
                .saturating_add(u128::from(current.vruntime()).saturating_mul(weight));
            total_weight = total_weight.saturating_add(weight);
        }
        (total_weight != 0)
            .then(|| u64::try_from(weighted_vruntime / total_weight).unwrap_or(u64::MAX))
    }

    fn add_weighted_entity(&mut self, entity: FairEntity) {
        let weight = u128::from(entity.weight());
        self.weighted_vruntime = self
            .weighted_vruntime
            .saturating_add(u128::from(entity.vruntime()).saturating_mul(weight));
        self.total_weight = self.total_weight.saturating_add(weight);
    }

    fn remove_weighted_entity(&mut self, entity: FairEntity) {
        let weight = u128::from(entity.weight());
        self.weighted_vruntime = self
            .weighted_vruntime
            .saturating_sub(u128::from(entity.vruntime()).saturating_mul(weight));
        self.total_weight = self.total_weight.saturating_sub(weight);
    }

    fn recycle_removed(&mut self, mut removed: Box<FairNode>) -> QueuedThread {
        let thread = removed
            .thread
            .take()
            .expect("removed fair node must still own its scheduling entity");
        removed.left = None;
        removed.right = self.spare.take();
        removed.height = 1;
        removed.min_vruntime = u64::MAX;
        self.spare = Some(removed);
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
        assert_eq!(summary.weighted_vruntime, self.weighted_vruntime);
        assert_eq!(summary.total_weight, self.total_weight);
    }
}

impl Drop for FairRunQueue {
    fn drop(&mut self) {
        // Recycled nodes form a right-linked free list. Drain it iteratively so
        // dropping a runqueue cannot recurse once per historical high-water node.
        while let Some(mut node) = self.spare.take() {
            self.spare = node.right.take();
        }
    }
}

fn fair_entity(thread: &QueuedThread) -> FairEntity {
    thread
        .entity
        .fair()
        .expect("FairRunQueue accepts only fair scheduling entities")
}

fn link_height(link: &FairLink) -> usize {
    link.as_deref().map_or(0, |node| node.height)
}

fn link_min_vruntime(link: &FairLink) -> u64 {
    link.as_deref().map_or(u64::MAX, |node| node.min_vruntime)
}

fn balance_factor(node: &FairNode) -> isize {
    link_height(&node.left) as isize - link_height(&node.right) as isize
}

fn rotate_left(mut root: Box<FairNode>) -> Box<FairNode> {
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

fn rotate_right(mut root: Box<FairNode>) -> Box<FairNode> {
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

fn rebalance(mut node: Box<FairNode>) -> Box<FairNode> {
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

fn insert_node(root: FairLink, inserted: Box<FairNode>) -> FairLink {
    let Some(mut root) = root else {
        return Some(inserted);
    };
    match inserted.key.cmp(&root.key) {
        Ordering::Less => root.left = insert_node(root.left.take(), inserted),
        Ordering::Greater => root.right = insert_node(root.right.take(), inserted),
        Ordering::Equal => panic!("fair runqueue key must be unique"),
    }
    Some(rebalance(root))
}

fn remove_node(root: FairLink, key: FairQueueKey) -> (FairLink, Option<Box<FairNode>>) {
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

fn take_min(mut root: Box<FairNode>) -> (FairLink, Box<FairNode>) {
    let Some(left) = root.left.take() else {
        let right = root.right.take();
        root.refresh();
        return (right, root);
    };
    let (left, minimum) = take_min(left);
    root.left = left;
    (Some(rebalance(root)), minimum)
}

fn earliest_eligible_key(node: Option<&FairNode>, virtual_time: u64) -> Option<FairQueueKey> {
    let node = node?;
    #[cfg(test)]
    record_fair_runqueue_visit();
    if node
        .left
        .as_deref()
        .is_some_and(|left| left.min_vruntime <= virtual_time)
    {
        return earliest_eligible_key(node.left.as_deref(), virtual_time);
    }
    if fair_entity(node.thread()).is_eligible(virtual_time) {
        return Some(node.key);
    }
    if node
        .right
        .as_deref()
        .is_some_and(|right| right.min_vruntime <= virtual_time)
    {
        return earliest_eligible_key(node.right.as_deref(), virtual_time);
    }
    None
}

fn find_first_matching<'queue>(
    node: Option<&'queue FairNode>,
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
    min_vruntime: u64,
    weighted_vruntime: u128,
    total_weight: u128,
}

#[cfg(test)]
fn validate_node(
    node: Option<&FairNode>,
    previous: &mut Option<FairQueueKey>,
) -> ValidationSummary {
    let Some(node) = node else {
        return ValidationSummary {
            count: 0,
            height: 0,
            min_vruntime: u64::MAX,
            weighted_vruntime: 0,
            total_weight: 0,
        };
    };
    let left = validate_node(node.left.as_deref(), previous);
    assert!(previous.is_none_or(|key| key < node.key));
    *previous = Some(node.key);
    let fair = fair_entity(node.thread());
    let right = validate_node(node.right.as_deref(), previous);
    let height = left.height.max(right.height).saturating_add(1);
    let min_vruntime = fair
        .vruntime()
        .min(left.min_vruntime)
        .min(right.min_vruntime);
    assert_eq!(node.height, height);
    assert_eq!(node.min_vruntime, min_vruntime);
    assert!(left.height.abs_diff(right.height) <= 1);
    let weight = u128::from(fair.weight());
    ValidationSummary {
        count: left.count.saturating_add(right.count).saturating_add(1),
        height,
        min_vruntime,
        weighted_vruntime: left
            .weighted_vruntime
            .saturating_add(right.weighted_vruntime)
            .saturating_add(u128::from(fair.vruntime()).saturating_mul(weight)),
        total_weight: left
            .total_weight
            .saturating_add(right.total_weight)
            .saturating_add(weight),
    }
}
