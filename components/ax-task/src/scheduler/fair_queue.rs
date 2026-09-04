//! Owner-local EEVDF runqueue with incremental lag accounting.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::cmp::Ordering;

use super::{
    queue::{QueuedThread, QueuedThreadSnapshot},
    virtual_before, virtual_delta, virtual_min,
};
use crate::{CpuSet, FairEntity, SchedulingEntity, ThreadId};

pub(super) enum FairPick {
    Runnable(QueuedThread),
    Delayed(Arc<crate::ThreadCore>),
}

enum FairEligible {
    Runnable(FairQueueKey),
    Delayed,
}

enum FairEligibleOwned {
    Runnable(Box<FairNode>),
    Delayed(Arc<crate::ThreadCore>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FairQueueKey {
    virtual_deadline: u64,
    sequence: u64,
    thread: ThreadId,
}

#[derive(Clone, Copy, Debug)]
struct FairMembership {
    generation: u32,
    key: FairQueueKey,
    delayed: bool,
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
pub(crate) struct FairNode {
    key: FairQueueKey,
    thread: Option<QueuedThread>,
    left: FairLink,
    right: FairLink,
    height: usize,
    min_vruntime: u64,
    min_service_request_ns: u64,
    max_service_request_ns: u64,
}

impl FairNode {
    pub(crate) fn empty() -> Box<Self> {
        Box::new(Self {
            key: FairQueueKey {
                virtual_deadline: 0,
                sequence: 0,
                thread: ThreadId::from_parts(0, 0),
            },
            thread: None,
            left: None,
            right: None,
            height: 1,
            min_vruntime: 0,
            min_service_request_ns: 0,
            max_service_request_ns: 0,
        })
    }

    fn reset(&mut self, thread: QueuedThread) {
        self.key = FairQueueKey::for_thread(&thread);
        let fair = fair_entity(&thread);
        self.min_vruntime = fair.vruntime();
        self.min_service_request_ns = fair.service_request_ns();
        self.max_service_request_ns = fair.service_request_ns();
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
        self.min_service_request_ns = link_min_service_request_ns(&self.left)
            .into_iter()
            .chain(link_min_service_request_ns(&self.right))
            .fold(fair_entity(self.thread()).service_request_ns(), u64::min);
        self.max_service_request_ns = link_max_service_request_ns(&self.left)
            .into_iter()
            .chain(link_max_service_request_ns(&self.right))
            .fold(fair_entity(self.thread()).service_request_ns(), u64::max);
    }
}

/// A fair-class queue ordered by virtual deadline and augmented by minimum
/// vruntime and service-request bounds. The augmentations keep eligible
/// selection logarithmic and expose Linux's cfs-rq slice bounds in O(1).
#[derive(Debug)]
pub(super) struct FairRunQueue {
    root: FairLink,
    keys: Vec<Option<FairMembership>>,
    zero_vruntime: u64,
    sum_weighted_delta: i128,
    total_weight: i128,
    migratable_count: usize,
    idle_count: usize,
    delayed_count: usize,
    len: usize,
}

impl FairRunQueue {
    pub(super) fn new() -> Self {
        Self {
            root: None,
            keys: Vec::new(),
            zero_vruntime: 0,
            sum_weighted_delta: 0,
            total_weight: 0,
            migratable_count: 0,
            idle_count: 0,
            delayed_count: 0,
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

    pub(super) const fn has_migratable(&self) -> bool {
        self.migratable_count != 0
    }

    pub(super) const fn migratable_count(&self) -> usize {
        self.migratable_count
    }

    /// Returns Linux `cfs_rq->h_nr_idle` for the flat owner runqueue.
    pub(super) const fn idle_count(&self) -> usize {
        self.idle_count
    }

    /// Returns Linux `cfs_rq->h_nr_delayed` for the flat owner runqueue.
    pub(super) const fn delayed_count(&self) -> usize {
        self.delayed_count
    }

    pub(super) fn total_weight(&self) -> u64 {
        u64::try_from(self.total_weight).expect("fair runqueue weight must remain non-negative")
    }

    pub(super) const fn virtual_time(&self) -> u64 {
        self.zero_vruntime
    }

    pub(super) fn min_service_request_ns(&self) -> Option<u64> {
        self.root.as_deref().map(|node| node.min_service_request_ns)
    }

    pub(super) fn max_service_request_ns(&self) -> Option<u64> {
        self.root.as_deref().map(|node| node.max_service_request_ns)
    }

    pub(super) fn insert(&mut self, thread: QueuedThread) {
        assert!(
            !fair_entity(&thread).is_delayed(),
            "ordinary Fair insertion cannot consume delayed dequeue state"
        );
        self.insert_entry(thread);
    }

    pub(super) fn insert_delayed(&mut self, thread: QueuedThread) {
        assert!(
            fair_entity(&thread).is_delayed(),
            "delayed Fair insertion requires entity-owned delayed state"
        );
        self.insert_entry(thread);
    }

    fn insert_entry(&mut self, thread: QueuedThread) {
        let key = FairQueueKey::for_thread(&thread);
        let delayed = fair_entity(&thread).is_delayed();
        let slot = thread.id.slot() as usize;
        assert!(
            self.keys.len() > slot,
            "thread construction must prepare fair rq membership"
        );
        assert!(
            self.keys[slot]
                .replace(FairMembership {
                    generation: thread.id.generation(),
                    key,
                    delayed,
                })
                .is_none(),
            "fair runqueue cannot contain one thread twice"
        );
        self.add_weighted_entity(fair_entity(&thread));
        if matches!(fair_entity(&thread).mode(), crate::FairMode::Idle) {
            self.idle_count = self
                .idle_count
                .checked_add(1)
                .expect("fair idle count must fit usize");
        }
        if delayed {
            self.delayed_count = self
                .delayed_count
                .checked_add(1)
                .expect("fair delayed count must fit usize");
        }
        if thread.migration_capable && !delayed {
            self.migratable_count = self
                .migratable_count
                .checked_add(1)
                .expect("fair migratable count must fit usize");
        }
        let mut inserted = unsafe {
            // SAFETY: target-rq placement serializes the single fair linkage
            // embedded in this thread's scheduler storage.
            thread.core.runqueue_nodes().take_fair()
        };
        inserted.reset(thread);
        self.root = insert_node(self.root.take(), inserted);
        self.len = self.len.saturating_add(1);
    }

    pub(super) fn remove(&mut self, thread: ThreadId) -> Option<QueuedThread> {
        self.remove_entry(thread)
    }

    fn remove_entry(&mut self, thread: ThreadId) -> Option<QueuedThread> {
        let key = self.membership(thread)?.key;
        self.remove_entry_with_key(thread, key, |_| true)
    }

    fn membership(&self, thread: ThreadId) -> Option<FairMembership> {
        self.keys
            .get(thread.slot() as usize)
            .and_then(|entry| *entry)
            .filter(|entry| entry.generation == thread.generation())
    }

    fn remove_entry_with_key(
        &mut self,
        thread: ThreadId,
        key: FairQueueKey,
        mut should_remove: impl FnMut(&FairNode) -> bool,
    ) -> Option<QueuedThread> {
        let indexed = self
            .keys
            .get(thread.slot() as usize)
            .and_then(|entry| *entry)?;
        if indexed.generation != thread.generation() || indexed.key != key {
            return None;
        }
        let (root, removed, found) = remove_node_if(self.root.take(), key, &mut should_remove);
        self.root = root;
        assert!(found, "fair runqueue identity index must match its tree");
        let removed = removed?;
        assert_eq!(removed.key, key, "fair removal must preserve indexed key");
        Some(self.finish_removed(removed))
    }

    fn finish_removed(&mut self, removed: Box<FairNode>) -> QueuedThread {
        let removed_thread = removed.thread();
        let slot = removed_thread.id.slot() as usize;
        let indexed = self
            .keys
            .get(slot)
            .and_then(|entry| *entry)
            .expect("removed fair node must remain indexed");
        assert_eq!(indexed.generation, removed_thread.id.generation());
        assert_eq!(indexed.key, removed.key);
        debug_assert_eq!(indexed.delayed, fair_entity(removed_thread).is_delayed());
        self.keys[slot] = None;
        let removed = Self::return_removed(removed);
        self.remove_weighted_entity(fair_entity(&removed));
        if matches!(fair_entity(&removed).mode(), crate::FairMode::Idle) {
            self.idle_count = self
                .idle_count
                .checked_sub(1)
                .expect("fair idle count must match queue membership");
        }
        if indexed.delayed {
            self.delayed_count = self
                .delayed_count
                .checked_sub(1)
                .expect("fair delayed count must match queue membership");
        }
        if removed.migration_capable && !indexed.delayed {
            self.migratable_count = self
                .migratable_count
                .checked_sub(1)
                .expect("fair migratable count must match queue membership");
        }
        self.len -= 1;
        removed
    }

    pub(super) fn pick_eligible(
        &mut self,
        virtual_time: u64,
        skip_delayed: bool,
    ) -> Option<FairPick> {
        let (root, selected) = take_earliest_eligible(self.root.take(), virtual_time, skip_delayed);
        self.root = root;
        match selected? {
            FairEligibleOwned::Delayed(core) => Some(FairPick::Delayed(core)),
            FairEligibleOwned::Runnable(node) => {
                Some(FairPick::Runnable(self.finish_removed(node)))
            }
        }
    }

    /// Returns the runnable current while Linux `RUN_TO_PARITY` still
    /// protects its active request. Linux checks `cfs_rq->curr` before the
    /// deadline tree in `pick_eevdf(..., protect=true)`; ax-task has already
    /// returned the outgoing current to this tree when selection begins, so
    /// its identity must be carried explicitly through the owner transaction.
    pub(super) fn take_protected_current(
        &mut self,
        current: ThreadId,
        virtual_time: u64,
    ) -> Option<QueuedThread> {
        let key = self.membership(current)?.key;
        self.remove_entry_with_key(current, key, |node| {
            protected_current_is_eligible(fair_entity(node.thread()), virtual_time)
        })
    }

    pub(super) fn earliest_eligible(&self, virtual_time: u64) -> Option<ThreadId> {
        earliest_eligible_key(self.root.as_deref(), virtual_time, true).map(|key| key.thread)
    }

    pub(super) fn is_delayed(&self, thread: ThreadId) -> bool {
        self.membership(thread).is_some_and(|entry| entry.delayed)
    }

    pub(super) fn take_delayed(&mut self, thread: ThreadId) -> Option<QueuedThread> {
        let membership = self.membership(thread)?;
        if !membership.delayed {
            return None;
        }
        self.remove_entry_with_key(thread, membership.key, |_| true)
    }

    pub(super) fn finish_delayed_dequeue(
        &mut self,
        thread: ThreadId,
        virtual_time: u64,
        timing_granularity_ns: u64,
    ) -> Option<QueuedThread> {
        let rq_max_slice_ns = self.max_service_request_ns()?;
        let mut thread = self.take_delayed(thread)?;
        let SchedulingEntity::Fair(fair) = thread.active.entity_mut() else {
            unreachable!("FairRunQueue can contain only Fair entities")
        };
        fair.finish_delayed_dequeue(virtual_time, rq_max_slice_ns, timing_granularity_ns)
            .ok()?;
        Some(thread)
    }

    pub(super) fn reactivate_delayed(
        &mut self,
        id: ThreadId,
        current: Option<FairEntity>,
        timing_granularity_ns: u64,
    ) -> Option<SchedulingEntity> {
        let membership = self.membership(id)?;
        if !membership.delayed {
            return None;
        }
        let key = membership.key;
        let fair = fair_entity(
            find_node(self.root.as_deref(), key)
                .expect("fair identity index must match its tree")
                .thread(),
        );
        let rq_max_slice_ns = self
            .max_service_request_ns()
            .unwrap_or(fair.service_request_ns())
            .max(current.map_or(0, FairEntity::service_request_ns));
        let requeue_lag = fair
            .delayed_requeue_lag(self.virtual_time(), rq_max_slice_ns, timing_granularity_ns)
            .ok()?;

        let Some(saved_lag) = requeue_lag else {
            let (entity, migration_capable) = {
                let node = find_node_mut(self.root.as_deref_mut(), key)
                    .expect("fair identity index must match its tree");
                let thread = node
                    .thread
                    .as_mut()
                    .expect("linked fair node must own one scheduling entity");
                let SchedulingEntity::Fair(fair) = thread.active.entity_mut() else {
                    unreachable!("FairRunQueue can contain only Fair entities")
                };
                fair.clear_delayed()
                    .expect("the indexed Fair entity was observed delayed under the same rq lock");
                self.keys[id.slot() as usize]
                    .as_mut()
                    .expect("reactivated fair entity must remain indexed")
                    .delayed = false;
                (thread.active.entity().clone(), thread.migration_capable)
            };
            if migration_capable {
                self.migratable_count = self
                    .migratable_count
                    .checked_add(1)
                    .expect("fair migratable count must fit usize");
            }
            self.delayed_count = self
                .delayed_count
                .checked_sub(1)
                .expect("fair delayed count must match queue membership");
            return Some(entity);
        };

        let mut thread = self.take_delayed(id)?;
        let placement_virtual_time = self.update_virtual_time(current);
        let runnable_weight = self
            .total_weight()
            .saturating_add(current.map_or(0, |entity| u64::from(entity.weight())));
        let SchedulingEntity::Fair(fair) = thread.active.entity_mut() else {
            unreachable!("FairRunQueue can contain only Fair entities")
        };
        fair.place_reactivated_delayed(placement_virtual_time, runnable_weight, saved_lag)
            .expect("a removed delayed Fair entity must retain delayed placement state");
        let entity = thread.active.entity().clone();
        self.insert(thread);
        Some(entity)
    }

    pub(super) fn update_affinity(&mut self, id: ThreadId, affinity: Arc<CpuSet>) -> bool {
        let Some(membership) = self.membership(id) else {
            return false;
        };
        let key = membership.key;
        let delayed = membership.delayed;
        let node = find_node_mut(self.root.as_deref_mut(), key)
            .expect("fair identity index must match its tree");
        debug_assert_eq!(delayed, fair_entity(node.thread()).is_delayed());
        let counted = node.thread().migration_capable && !delayed;
        let migration_capable = affinity.is_migration_capable();
        let next_counted = migration_capable && !delayed;
        let thread = node
            .thread
            .as_mut()
            .expect("linked fair node must own one scheduling entity");
        thread.update_affinity(affinity);
        thread.migration_capable = migration_capable;
        match (counted, next_counted) {
            (false, true) => self.migratable_count += 1,
            (true, false) => {
                self.migratable_count = self
                    .migratable_count
                    .checked_sub(1)
                    .expect("fair migratable count must match queue membership")
            }
            _ => {}
        }
        true
    }

    pub(super) fn mark_balance_candidate(&mut self, id: ThreadId, scan_epoch: u64) -> bool {
        let Some(membership) = self.membership(id) else {
            return false;
        };
        let key = membership.key;
        let delayed = membership.delayed;
        let node = find_node_mut(self.root.as_deref_mut(), key)
            .expect("fair identity index must match its tree");
        debug_assert_eq!(delayed, fair_entity(node.thread()).is_delayed());
        if delayed {
            return false;
        }
        node.thread
            .as_mut()
            .expect("linked fair node must own one scheduling entity")
            .balance_scan_epoch = scan_epoch;
        true
    }

    pub(super) fn find_first_matching(
        &self,
        predicate: &mut impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThreadSnapshot> {
        find_first_matching(self.root.as_deref(), predicate).map(QueuedThreadSnapshot::from)
    }

    pub(super) fn find_first_migratable_matching(
        &self,
        predicate: &mut impl FnMut(&QueuedThread) -> bool,
    ) -> Option<QueuedThreadSnapshot> {
        find_first_runnable_matching(self.root.as_deref(), predicate)
            .map(QueuedThreadSnapshot::from)
    }

    pub(super) fn update_virtual_time(&mut self, current: Option<FairEntity>) -> u64 {
        let mut sum_weighted_delta = self.sum_weighted_delta;
        let mut total_weight = self.total_weight;
        if let Some(current) = current {
            let weight = i128::from(current.weight());
            sum_weighted_delta += self.entity_delta(current) * weight;
            total_weight += weight;
        }
        if total_weight == 0 {
            return self.zero_vruntime;
        }

        // Rust integer division truncates toward zero. EEVDF needs the same
        // left-biased average as Linux so an entity exactly at V is eligible.
        if sum_weighted_delta < 0 {
            sum_weighted_delta -= total_weight - 1;
        }
        let delta = divide_weighted_sum(sum_weighted_delta, total_weight);
        let delta = i64::try_from(delta).expect("a weighted mean of i64 deltas must fit in i64");
        self.rebase(delta);
        self.zero_vruntime
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

    fn return_removed(mut removed: Box<FairNode>) -> QueuedThread {
        let thread = removed
            .thread
            .take()
            .expect("removed fair node must still own its scheduling entity");
        removed.left = None;
        removed.right = None;
        removed.height = 1;
        removed.min_vruntime = 0;
        removed.min_service_request_ns = 0;
        removed.max_service_request_ns = 0;
        unsafe {
            // SAFETY: the node is physically unlinked before the placement
            // transaction can publish this thread to another runqueue.
            thread.core.runqueue_nodes().return_fair(removed);
        }
        thread
    }
}

fn fair_entity(thread: &QueuedThread) -> FairEntity {
    thread
        .active
        .entity()
        .fair()
        .expect("FairRunQueue accepts only fair scheduling entities")
}

fn protected_current_is_eligible(entity: FairEntity, virtual_time: u64) -> bool {
    !entity.is_delayed() && entity.is_eligible(virtual_time) && entity.slice_is_protected()
}

fn link_height(link: &FairLink) -> usize {
    link.as_deref().map_or(0, |node| node.height)
}

const fn weighted_sum_needs_wide_division(sum: i128, total_weight: i128) -> bool {
    sum < i64::MIN as i128
        || sum > i64::MAX as i128
        || total_weight <= 0
        || total_weight > i64::MAX as i128
}

fn divide_weighted_sum(sum: i128, total_weight: i128) -> i128 {
    if weighted_sum_needs_wide_division(sum, total_weight) {
        return sum / total_weight;
    }

    // The bounds above make both casts value-preserving. Linux's
    // `avg_vruntime()` performs this ordinary case with `s64 / long`; retain
    // the wider representation only as an overflow-safe fallback.
    i128::from((sum as i64) / (total_weight as i64))
}

fn link_min_vruntime(link: &FairLink) -> Option<u64> {
    link.as_deref().map(|node| node.min_vruntime)
}

fn link_min_service_request_ns(link: &FairLink) -> Option<u64> {
    link.as_deref().map(|node| node.min_service_request_ns)
}

fn link_max_service_request_ns(link: &FairLink) -> Option<u64> {
    link.as_deref().map(|node| node.max_service_request_ns)
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

fn remove_node_if(
    root: FairLink,
    key: FairQueueKey,
    should_remove: &mut impl FnMut(&FairNode) -> bool,
) -> (FairLink, Option<Box<FairNode>>, bool) {
    let Some(mut root) = root else {
        return (None, None, false);
    };
    match key.cmp(&root.key) {
        Ordering::Less => {
            let (left, removed, found) = remove_node_if(root.left.take(), key, should_remove);
            if removed.is_some() {
                root.left = left;
                (Some(rebalance(root)), removed, found)
            } else {
                root.left = left;
                (Some(root), None, found)
            }
        }
        Ordering::Greater => {
            let (right, removed, found) = remove_node_if(root.right.take(), key, should_remove);
            if removed.is_some() {
                root.right = right;
                (Some(rebalance(root)), removed, found)
            } else {
                root.right = right;
                (Some(root), None, found)
            }
        }
        Ordering::Equal => {
            if !should_remove(&root) {
                return (Some(root), None, true);
            }
            let (remaining, removed) = remove_root(root);
            (remaining, Some(removed), true)
        }
    }
}

fn remove_root(mut root: Box<FairNode>) -> (FairLink, Box<FairNode>) {
    match (root.left.take(), root.right.take()) {
        (None, right) => (right, root),
        (left, None) => (left, root),
        (Some(left), Some(right)) => {
            let (right, mut successor) = take_min(right);
            successor.left = Some(left);
            successor.right = right;
            (Some(rebalance(successor)), root)
        }
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

fn earliest_eligible(
    node: Option<&FairNode>,
    virtual_time: u64,
    skip_delayed: bool,
) -> Option<FairEligible> {
    let node = node?;
    if node
        .left
        .as_deref()
        .is_some_and(|left| !virtual_before(virtual_time, left.min_vruntime))
        && let Some(candidate) = earliest_eligible(node.left.as_deref(), virtual_time, skip_delayed)
    {
        return Some(candidate);
    }
    if fair_entity(node.thread()).is_eligible(virtual_time)
        && !(skip_delayed && fair_entity(node.thread()).is_delayed())
    {
        return Some(if fair_entity(node.thread()).is_delayed() {
            FairEligible::Delayed
        } else {
            FairEligible::Runnable(node.key)
        });
    }
    if node
        .right
        .as_deref()
        .is_some_and(|right| !virtual_before(virtual_time, right.min_vruntime))
    {
        return earliest_eligible(node.right.as_deref(), virtual_time, skip_delayed);
    }
    None
}

fn take_earliest_eligible(
    root: FairLink,
    virtual_time: u64,
    skip_delayed: bool,
) -> (FairLink, Option<FairEligibleOwned>) {
    let Some(mut root) = root else {
        return (None, None);
    };
    if root
        .left
        .as_deref()
        .is_some_and(|left| !virtual_before(virtual_time, left.min_vruntime))
    {
        let (left, selected) = take_earliest_eligible(root.left.take(), virtual_time, skip_delayed);
        root.left = left;
        if selected.is_some() {
            return (Some(rebalance(root)), selected);
        }
    }
    let entity = fair_entity(root.thread());
    if entity.is_eligible(virtual_time) && !(skip_delayed && entity.is_delayed()) {
        if entity.is_delayed() {
            let core = Arc::clone(&root.thread().core);
            return (Some(root), Some(FairEligibleOwned::Delayed(core)));
        }
        let (remaining, removed) = remove_root(root);
        return (remaining, Some(FairEligibleOwned::Runnable(removed)));
    }
    if root
        .right
        .as_deref()
        .is_some_and(|right| !virtual_before(virtual_time, right.min_vruntime))
    {
        let (right, selected) =
            take_earliest_eligible(root.right.take(), virtual_time, skip_delayed);
        root.right = right;
        if selected.is_some() {
            return (Some(rebalance(root)), selected);
        }
    }
    (Some(root), None)
}

fn earliest_eligible_key(
    node: Option<&FairNode>,
    virtual_time: u64,
    skip_delayed: bool,
) -> Option<FairQueueKey> {
    match earliest_eligible(node, virtual_time, skip_delayed)? {
        FairEligible::Runnable(key) => Some(key),
        FairEligible::Delayed => None,
    }
}

fn find_node(node: Option<&FairNode>, key: FairQueueKey) -> Option<&FairNode> {
    let node = node?;
    match key.cmp(&node.key) {
        Ordering::Less => find_node(node.left.as_deref(), key),
        Ordering::Greater => find_node(node.right.as_deref(), key),
        Ordering::Equal => Some(node),
    }
}

fn find_node_mut(node: Option<&mut FairNode>, key: FairQueueKey) -> Option<&mut FairNode> {
    let node = node?;
    match key.cmp(&node.key) {
        Ordering::Less => find_node_mut(node.left.as_deref_mut(), key),
        Ordering::Greater => find_node_mut(node.right.as_deref_mut(), key),
        Ordering::Equal => Some(node),
    }
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

fn find_first_runnable_matching<'queue>(
    node: Option<&'queue FairNode>,
    predicate: &mut impl FnMut(&QueuedThread) -> bool,
) -> Option<&'queue QueuedThread> {
    let node = node?;
    find_first_runnable_matching(node.left.as_deref(), predicate)
        .or_else(|| {
            (!fair_entity(node.thread()).is_delayed() && predicate(node.thread()))
                .then_some(node.thread())
        })
        .or_else(|| find_first_runnable_matching(node.right.as_deref(), predicate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FairMode, Nice};

    #[test]
    fn ordinary_weighted_sum_uses_native_width_division() {
        assert!(!weighted_sum_needs_wide_division(-40_960, 40_960));
        assert_eq!(divide_weighted_sum(-40_960, 40_960), -1);
        assert!(weighted_sum_needs_wide_division(
            i128::from(i64::MAX) + 1,
            1
        ));
        assert_eq!(
            divide_weighted_sum(i128::from(i64::MAX) + 1, 2),
            (i128::from(i64::MAX) + 1) / 2
        );
    }

    #[test]
    fn linux_run_to_parity_keeps_an_eligible_protected_current() {
        let mut current = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 1_000);
        current.set_slice_protection(None);

        assert!(protected_current_is_eligible(current, 1_000));
    }
}
