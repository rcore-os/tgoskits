//! Owner-local EEVDF runqueue with incremental lag accounting.

use alloc::{boxed::Box, vec::Vec};
use core::cmp::Ordering;

#[cfg(test)]
use super::queue::record_fair_runqueue_visit;
use super::{queue::QueuedThread, virtual_before, virtual_delta, virtual_min};
use crate::{FairEntity, ThreadId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

impl Ord for FairQueueKey {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.virtual_deadline == other.virtual_deadline {
            self.sequence
                .cmp(&other.sequence)
                .then_with(|| self.thread.cmp(&other.thread))
        } else if virtual_before(self.virtual_deadline, other.virtual_deadline) {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    }
}

impl PartialOrd for FairQueueKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
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
        let mut min_vruntime = fair_entity(self.thread()).vruntime();
        if let Some(left) = link_min_vruntime(&self.left) {
            min_vruntime = virtual_min(min_vruntime, left);
        }
        if let Some(right) = link_min_vruntime(&self.right) {
            min_vruntime = virtual_min(min_vruntime, right);
        }
        self.min_vruntime = min_vruntime;
    }
}

/// A fair-class queue ordered by virtual deadline and augmented by minimum
/// vruntime. The augmentation makes earliest-eligible selection logarithmic.
#[derive(Debug)]
pub(super) struct FairRunQueue {
    root: FairLink,
    spare: FairLink,
    keys: Vec<Option<(u32, FairQueueKey)>>,
    zero_vruntime: u64,
    sum_weighted_delta: i128,
    total_weight: i128,
    len: usize,
}

impl FairRunQueue {
    pub(super) fn new() -> Self {
        Self {
            root: None,
            spare: None,
            keys: Vec::new(),
            zero_vruntime: 0,
            sum_weighted_delta: 0,
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

    pub(super) fn weighted_virtual_time(&mut self, current: Option<FairEntity>) -> Option<u64> {
        let mut sum_weighted_delta = self.sum_weighted_delta;
        let mut total_weight = self.total_weight;
        if let Some(current) = current {
            let weight = i128::from(current.weight());
            sum_weighted_delta += self.entity_delta(current) * weight;
            total_weight += weight;
        }
        if total_weight == 0 {
            return None;
        }

        // Rust integer division truncates toward zero. EEVDF needs the same
        // left-biased average as Linux so an entity exactly at V is eligible.
        if sum_weighted_delta < 0 {
            sum_weighted_delta -= total_weight - 1;
        }
        let delta = sum_weighted_delta / total_weight;
        let delta = i64::try_from(delta).expect("a weighted mean of i64 deltas must fit in i64");
        self.rebase(delta);
        Some(self.zero_vruntime)
    }

    fn add_weighted_entity(&mut self, entity: FairEntity) {
        if self.total_weight == 0 {
            self.zero_vruntime = entity.vruntime();
            self.sum_weighted_delta = 0;
        }
        let weight = i128::from(entity.weight());
        self.sum_weighted_delta += self.entity_delta(entity) * weight;
        self.total_weight += weight;
    }

    fn remove_weighted_entity(&mut self, entity: FairEntity) {
        let weight = i128::from(entity.weight());
        self.sum_weighted_delta -= self.entity_delta(entity) * weight;
        self.total_weight -= weight;
    }

    fn entity_delta(&self, entity: FairEntity) -> i128 {
        i128::from(virtual_delta(entity.vruntime(), self.zero_vruntime))
    }

    fn rebase(&mut self, delta: i64) {
        self.sum_weighted_delta -= i128::from(delta) * self.total_weight;
        self.zero_vruntime = self.zero_vruntime.wrapping_add(delta as u64);
    }

    fn recycle_removed(&mut self, mut removed: Box<FairNode>) -> QueuedThread {
        let thread = removed
            .thread
            .take()
            .expect("removed fair node must still own its scheduling entity");
        removed.left = None;
        removed.right = self.spare.take();
        removed.height = 1;
        removed.min_vruntime = 0;
        self.spare = Some(removed);
        thread
    }

    #[cfg(test)]
    pub(super) fn assert_invariants(&self) {
        let mut previous = None;
        let summary = validate_node(self.root.as_deref(), &mut previous, self.zero_vruntime);
        assert_eq!(summary.count, self.len);
        assert_eq!(
            self.keys.iter().filter(|entry| entry.is_some()).count(),
            self.len
        );
        assert_eq!(summary.sum_weighted_delta, self.sum_weighted_delta);
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

fn link_min_vruntime(link: &FairLink) -> Option<u64> {
    link.as_deref().map(|node| node.min_vruntime)
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
        .is_some_and(|left| !virtual_before(virtual_time, left.min_vruntime))
    {
        return earliest_eligible_key(node.left.as_deref(), virtual_time);
    }
    if fair_entity(node.thread()).is_eligible(virtual_time) {
        return Some(node.key);
    }
    if node
        .right
        .as_deref()
        .is_some_and(|right| !virtual_before(virtual_time, right.min_vruntime))
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
    min_vruntime: Option<u64>,
    sum_weighted_delta: i128,
    total_weight: i128,
}

#[cfg(test)]
fn validate_node(
    node: Option<&FairNode>,
    previous: &mut Option<FairQueueKey>,
    zero_vruntime: u64,
) -> ValidationSummary {
    let Some(node) = node else {
        return ValidationSummary {
            count: 0,
            height: 0,
            min_vruntime: None,
            sum_weighted_delta: 0,
            total_weight: 0,
        };
    };
    let left = validate_node(node.left.as_deref(), previous, zero_vruntime);
    assert!(previous.is_none_or(|key| key < node.key));
    *previous = Some(node.key);
    let fair = fair_entity(node.thread());
    let right = validate_node(node.right.as_deref(), previous, zero_vruntime);
    let height = left.height.max(right.height).saturating_add(1);
    let min_vruntime = left
        .min_vruntime
        .into_iter()
        .chain(right.min_vruntime)
        .fold(fair.vruntime(), virtual_min);
    assert_eq!(node.height, height);
    assert_eq!(node.min_vruntime, min_vruntime);
    assert!(left.height.abs_diff(right.height) <= 1);
    let weight = i128::from(fair.weight());
    ValidationSummary {
        count: left.count.saturating_add(right.count).saturating_add(1),
        height,
        min_vruntime: Some(min_vruntime),
        sum_weighted_delta: left.sum_weighted_delta
            + right.sum_weighted_delta
            + i128::from(virtual_delta(fair.vruntime(), zero_vruntime)) * weight,
        total_weight: left.total_weight + right.total_weight + weight,
    }
}
