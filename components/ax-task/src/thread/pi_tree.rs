//! Allocation-free ordered PI waiter linkage.

use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use core::{cell::UnsafeCell, cmp::Ordering, fmt, ptr::NonNull};

use crate::{SchedulePolicy, SchedulingUrgency, ThreadCore, ThreadId};

/// Effective donation cloned into an rtmutex waiter node.
///
/// Linux stores cloned priority/deadline ordering in `rt_mutex_waiter` and
/// retains the donating task identity through `pi_top_task`. Keeping the same
/// facts in the preallocated node lets the owner consume one coherent snapshot
/// under its PI lock without taking another task lock or consulting the global
/// registry.
#[derive(Clone, Debug)]
pub(crate) struct PiDonation {
    pub(crate) policy: SchedulePolicy,
    pub(crate) root: ThreadId,
    pub(crate) boost_urgency: SchedulingUrgency,
    wait_generation: Option<u64>,
    waiter_core: Weak<ThreadCore>,
    pub(crate) root_core: Weak<ThreadCore>,
}

impl PiDonation {
    pub(crate) fn new(
        policy: SchedulePolicy,
        root: ThreadId,
        boost_urgency: SchedulingUrgency,
        waiter_core: &Arc<ThreadCore>,
        root_core: &Arc<ThreadCore>,
    ) -> Self {
        Self {
            policy,
            root,
            boost_urgency,
            wait_generation: None,
            waiter_core: Arc::downgrade(waiter_core),
            root_core: Arc::downgrade(root_core),
        }
    }

    /// Binds this snapshot to the committed physical-lock waiter generation.
    pub(crate) const fn with_wait_generation(mut self, generation: u64) -> Self {
        self.wait_generation = Some(generation);
        self
    }

    /// Returns the generation protected by the containing mutex wait lock.
    pub(crate) const fn wait_generation(&self) -> Option<u64> {
        self.wait_generation
    }

    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.policy == other.policy
            && self.root == other.root
            && self.boost_urgency == other.boost_urgency
    }

    pub(crate) fn waiter_core(&self) -> Option<Arc<ThreadCore>> {
        self.waiter_core.upgrade()
    }
}

/// Stable ordering copied into both the lock waiter tree and owner donor tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PiWaitKey {
    pub(crate) urgency: SchedulingUrgency,
    pub(crate) sequence: u64,
    pub(crate) thread: ThreadId,
}

impl PiWaitKey {
    pub(crate) const fn new(urgency: SchedulingUrgency, sequence: u64, thread: ThreadId) -> Self {
        Self {
            urgency,
            sequence,
            thread,
        }
    }
}

impl Ord for PiWaitKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.urgency
            .cmp(&other.urgency)
            .then_with(|| self.sequence.cmp(&other.sequence))
            .then_with(|| self.thread.cmp(&other.thread))
    }
}

impl PartialOrd for PiWaitKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

type PiWaitLink = Option<Box<PiWaitNode>>;

/// One preallocated AVL linkage owned by a blocked thread.
pub(crate) struct PiWaitNode {
    key: PiWaitKey,
    donation: Option<PiDonation>,
    left: PiWaitLink,
    right: PiWaitLink,
    height: usize,
}

impl PiWaitNode {
    fn empty() -> Box<Self> {
        Box::new(Self {
            key: PiWaitKey::new(
                SchedulingUrgency::new(u8::MAX, u64::MAX),
                u64::MAX,
                ThreadId::from_parts(0, 0),
            ),
            donation: None,
            left: None,
            right: None,
            height: 1,
        })
    }

    fn reset(&mut self, key: PiWaitKey, donation: PiDonation) {
        self.key = key;
        self.donation = Some(donation);
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

impl fmt::Debug for PiWaitNode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PiWaitNode")
            .field("key", &self.key)
            .field("donation", &self.donation)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// The two independent tree links required by Linux-style PI ownership.
///
/// One link belongs to the mutex waiter tree. The other is linked only while
/// this waiter is the top waiter of a lock owned by another thread.
pub(crate) struct PiWaitNodeStorage {
    lock_waiter: UnsafeCell<Option<Box<PiWaitNode>>>,
    owner_donor: UnsafeCell<Option<Box<PiWaitNode>>>,
}

impl PiWaitNodeStorage {
    pub(crate) fn new() -> Self {
        Self {
            lock_waiter: UnsafeCell::new(Some(PiWaitNode::empty())),
            owner_donor: UnsafeCell::new(Some(PiWaitNode::empty())),
        }
    }

    pub(crate) unsafe fn take_lock_waiter(&self) -> Box<PiWaitNode> {
        unsafe { &mut *self.lock_waiter.get() }
            .take()
            .expect("one thread cannot wait on two PI locks")
    }

    pub(crate) unsafe fn return_lock_waiter(&self, node: Box<PiWaitNode>) {
        assert!(
            unsafe { &mut *self.lock_waiter.get() }
                .replace(node)
                .is_none(),
            "unlinked PI waiter must have one storage owner"
        );
    }

    pub(crate) unsafe fn take_owner_donor(&self) -> Box<PiWaitNode> {
        unsafe { &mut *self.owner_donor.get() }
            .take()
            .expect("one PI waiter can donate through only one lock owner")
    }

    pub(crate) unsafe fn return_owner_donor(&self, node: Box<PiWaitNode>) {
        assert!(
            unsafe { &mut *self.owner_donor.get() }
                .replace(node)
                .is_none(),
            "unlinked PI donor must have one storage owner"
        );
    }
}

// SAFETY: the task-system PI transaction serializes both link transfers. A
// linked node is absent from this storage and cannot be taken a second time.
unsafe impl Sync for PiWaitNodeStorage {}

impl fmt::Debug for PiWaitNodeStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PiWaitNodeStorage")
            .finish_non_exhaustive()
    }
}

/// Cached ordered set used for one lock's waiters or one owner's lock tops.
#[derive(Debug)]
pub(crate) struct PiWaitTree {
    root: PiWaitLink,
    first: Option<NonNull<PiWaitNode>>,
    len: usize,
}

impl PiWaitTree {
    pub(crate) const fn new() -> Self {
        Self {
            root: None,
            first: None,
            len: 0,
        }
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn first(&self) -> Option<PiWaitKey> {
        self.first.map(|first| {
            // SAFETY: `first` points into one of the boxes owned by `root`.
            // AVL rotations move boxes, never their allocations, and removal
            // refreshes this cache before returning the detached box.
            unsafe { first.as_ref().key }
        })
    }

    pub(crate) fn first_entry(&self) -> Option<(PiWaitKey, PiDonation)> {
        self.first.map(|first| {
            // SAFETY: identical ownership contract to `first()`.
            let first = unsafe { first.as_ref() };
            (
                first.key,
                first
                    .donation
                    .as_ref()
                    .expect("linked PI waiter must retain its donation")
                    .clone(),
            )
        })
    }

    /// Returns the least urgent-key entry other than `excluded` without
    /// changing either intrusive linkage.
    ///
    /// PI owner updates use this to validate the prospective `pi_waiters`
    /// top before replacing one lock's contribution, matching Linux's rule
    /// that a failed `rt_mutex_setprio()` transaction cannot partially mutate
    /// the owner tree.
    pub(crate) fn first_entry_excluding(
        &self,
        excluded: Option<PiWaitKey>,
    ) -> Option<(PiWaitKey, PiDonation)> {
        let first = find_first_excluding(self.root.as_deref(), excluded)?;
        Some((
            first.key,
            first
                .donation
                .as_ref()
                .expect("linked PI waiter must retain its donation")
                .clone(),
        ))
    }

    pub(crate) fn donation(&self, key: PiWaitKey) -> Option<PiDonation> {
        find_node(self.root.as_deref(), key).map(|node| {
            node.donation
                .as_ref()
                .expect("linked PI waiter must retain its donation")
                .clone()
        })
    }

    pub(crate) fn contains(&self, key: PiWaitKey) -> bool {
        find_node(self.root.as_deref(), key).is_some()
    }

    pub(crate) fn insert(
        &mut self,
        key: PiWaitKey,
        donation: PiDonation,
        mut node: Box<PiWaitNode>,
    ) {
        node.reset(key, donation);
        let node_pointer = NonNull::from(node.as_mut());
        self.root = insert_node(self.root.take(), node);
        if self.first().is_none_or(|first| key < first) {
            self.first = Some(node_pointer);
        }
        self.len = self
            .len
            .checked_add(1)
            .expect("PI waiter tree length overflow");
    }

    pub(crate) fn remove(&mut self, key: PiWaitKey) -> Option<Box<PiWaitNode>> {
        let (root, removed) = remove_node(self.root.take(), key);
        self.root = root;
        if removed.is_some() {
            self.len -= 1;
            if self.first() == Some(key) {
                self.first = find_first(self.root.as_deref()).map(NonNull::from);
            }
        }
        removed
    }
}

// SAFETY: cached pointers refer only to heap nodes owned by the same tree.
// Moving the tree preserves those allocations, and every mutation requires
// exclusive access through the PI graph transaction.
unsafe impl Send for PiWaitTree {}

fn link_height(link: &PiWaitLink) -> usize {
    link.as_deref().map_or(0, |node| node.height)
}

fn balance_factor(node: &PiWaitNode) -> isize {
    link_height(&node.left) as isize - link_height(&node.right) as isize
}

fn rotate_left(mut root: Box<PiWaitNode>) -> Box<PiWaitNode> {
    let mut promoted = root
        .right
        .take()
        .expect("left rotation requires a right PI waiter child");
    root.right = promoted.left.take();
    root.refresh();
    promoted.left = Some(root);
    promoted.refresh();
    promoted
}

fn rotate_right(mut root: Box<PiWaitNode>) -> Box<PiWaitNode> {
    let mut promoted = root
        .left
        .take()
        .expect("right rotation requires a left PI waiter child");
    root.left = promoted.right.take();
    root.refresh();
    promoted.right = Some(root);
    promoted.refresh();
    promoted
}

fn rebalance(mut node: Box<PiWaitNode>) -> Box<PiWaitNode> {
    node.refresh();
    match balance_factor(&node) {
        factor if factor > 1 => {
            if node
                .left
                .as_deref()
                .is_some_and(|left| balance_factor(left) < 0)
            {
                let left = node.left.take().expect("PI balance requires left child");
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
                let right = node.right.take().expect("PI balance requires right child");
                node.right = Some(rotate_right(right));
            }
            rotate_left(node)
        }
        _ => node,
    }
}

fn insert_node(root: PiWaitLink, inserted: Box<PiWaitNode>) -> PiWaitLink {
    let Some(mut root) = root else {
        return Some(inserted);
    };
    match inserted.key.cmp(&root.key) {
        Ordering::Less => root.left = insert_node(root.left.take(), inserted),
        Ordering::Greater => root.right = insert_node(root.right.take(), inserted),
        Ordering::Equal => panic!("PI waiter tree key must be unique"),
    }
    Some(rebalance(root))
}

fn remove_node(root: PiWaitLink, key: PiWaitKey) -> (PiWaitLink, Option<Box<PiWaitNode>>) {
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

fn take_min(mut root: Box<PiWaitNode>) -> (PiWaitLink, Box<PiWaitNode>) {
    let Some(left) = root.left.take() else {
        let right = root.right.take();
        root.refresh();
        return (right, root);
    };
    let (left, minimum) = take_min(left);
    root.left = left;
    (Some(rebalance(root)), minimum)
}

fn find_node(node: Option<&PiWaitNode>, key: PiWaitKey) -> Option<&PiWaitNode> {
    let node = node?;
    match key.cmp(&node.key) {
        Ordering::Less => find_node(node.left.as_deref(), key),
        Ordering::Greater => find_node(node.right.as_deref(), key),
        Ordering::Equal => Some(node),
    }
}

fn find_first(node: Option<&PiWaitNode>) -> Option<&PiWaitNode> {
    let mut current = node?;
    while let Some(left) = current.left.as_deref() {
        current = left;
    }
    Some(current)
}

fn find_first_excluding(
    node: Option<&PiWaitNode>,
    excluded: Option<PiWaitKey>,
) -> Option<&PiWaitNode> {
    let node = node?;
    find_first_excluding(node.left.as_deref(), excluded)
        .or_else(|| (Some(node.key) != excluded).then_some(node))
        .or_else(|| find_first_excluding(node.right.as_deref(), excluded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FairMode, Nice};

    fn donation(generation: u64) -> PiDonation {
        PiDonation {
            policy: SchedulePolicy::Fair {
                nice: Nice::ZERO,
                mode: FairMode::Normal,
            },
            root: ThreadId::from_parts(1, 0),
            boost_urgency: SchedulingUrgency::new(3, 0),
            wait_generation: None,
            waiter_core: Weak::new(),
            root_core: Weak::new(),
        }
        .with_wait_generation(generation)
    }

    #[test]
    fn waiter_tree_snapshot_retains_the_committed_generation() {
        let key = PiWaitKey::new(SchedulingUrgency::new(3, 0), 7, ThreadId::from_parts(2, 0));
        let mut tree = PiWaitTree::new();
        tree.insert(key, donation(11), PiWaitNode::empty());

        let (_, snapshot) = tree.first_entry().expect("inserted waiter must be first");
        assert_eq!(snapshot.wait_generation(), Some(11));
        assert!(snapshot.same_source(&donation(12)));
    }
}
