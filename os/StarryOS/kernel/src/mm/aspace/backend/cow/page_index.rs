//! Source-local COW identities with lock-external node allocation and retirement.

use alloc::boxed::Box;
use core::cmp::Ordering;

use super::{
    Arc, CowPageIndexEntry, CowPageIndexOwner, PAGE_SIZE_4K, PageObject, PhysAddr, StarryError,
    StarryResult,
};

type Link = Option<Box<Node>>;

struct Node {
    // Only a caller-owned, not-yet-installed reservation may have no entry.
    entry: Option<CowPageIndexEntry>,
    left: Link,
    right: Link,
    height: usize,
}

// Growing the index must remain possible with isolated free base pages.
const _: () = assert!(core::mem::size_of::<Node>() <= PAGE_SIZE_4K);

impl Node {
    fn entry(&self) -> &CowPageIndexEntry {
        self.entry
            .as_ref()
            .expect("installed COW node owns an entry")
    }

    fn entry_mut(&mut self) -> &mut CowPageIndexEntry {
        self.entry
            .as_mut()
            .expect("installed COW node owns an entry")
    }

    fn key(&self) -> usize {
        self.entry().paddr.as_usize()
    }

    fn refresh(&mut self) {
        self.height = 1 + height(&self.left).max(height(&self.right));
    }
}

/// Owns unused storage and displaced owners until the index guard is released.
pub(super) struct CowPageIndexReservation {
    prepared: Link,
    retired: Link,
    retired_owner: Option<CowPageIndexOwner>,
}

impl CowPageIndexReservation {
    pub(super) fn try_with_capacity(capacity: usize) -> StarryResult<Self> {
        let prepared = match capacity {
            0 => None,
            1 => Some(
                Box::try_new(Node {
                    entry: None,
                    left: None,
                    right: None,
                    height: 1,
                })
                .map_err(|_| StarryError::NoMemory)?,
            ),
            _ => return Err(StarryError::BadState),
        };
        Ok(Self {
            prepared,
            retired: None,
            retired_owner: None,
        })
    }

    fn retire(&mut self, mut node: Box<Node>) {
        debug_assert!(node.left.is_none() && node.right.is_none());
        node.right = self.retired.take();
        self.retired = Some(node);
    }
}

impl Drop for CowPageIndexReservation {
    fn drop(&mut self) {
        // Unlike the balanced live tree, a retirement chain may be arbitrarily
        // deep when a huge extent replaces many expired base-page identities.
        while let Some(mut node) = self.retired.take() {
            self.retired = node.right.take();
        }
    }
}

pub(super) enum CowPageIndexInsertError {
    StaleReservation,
    Invalid(StarryError),
}

pub(super) struct CowPageIndex {
    root: Link,
    prune_after: Option<usize>,
    pub(super) zero_page: Option<Arc<PageObject>>,
}

impl CowPageIndex {
    pub(super) const fn new() -> Self {
        Self {
            root: None,
            prune_after: None,
            zero_page: None,
        }
    }

    #[cfg(all(test, axtest))]
    pub(super) fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    fn exact(&self, key: usize) -> Option<&Node> {
        self.lower_bound(key).filter(|node| node.key() == key)
    }

    fn exact_mut(&mut self, key: usize) -> Option<&mut Node> {
        let mut link = self.root.as_deref_mut();
        while let Some(node) = link {
            match key.cmp(&node.key()) {
                Ordering::Less => link = node.left.as_deref_mut(),
                Ordering::Greater => link = node.right.as_deref_mut(),
                Ordering::Equal => return Some(node),
            }
        }
        None
    }

    fn lower_bound(&self, key: usize) -> Option<&Node> {
        let mut link = self.root.as_deref();
        let mut found = None;
        while let Some(node) = link {
            if node.key() >= key {
                found = Some(node);
                link = node.left.as_deref();
            } else {
                link = node.right.as_deref();
            }
        }
        found
    }

    fn predecessor(&self, key: usize) -> Option<&Node> {
        let mut link = self.root.as_deref();
        let mut found = None;
        while let Some(node) = link {
            if node.key() <= key {
                found = Some(node);
                link = node.right.as_deref();
            } else {
                link = node.left.as_deref();
            }
        }
        found
    }

    fn first_overlap(&self, start: usize, end: usize) -> Option<&Node> {
        if let Some(node) = self.predecessor(start)
            && node.entry().end().is_none_or(|after| start < after)
        {
            return Some(node);
        }
        self.lower_bound(start).filter(|node| node.key() < end)
    }

    /// Returns a node count, never storage proportional to the live index.
    pub(super) fn insert_reservation_capacity(
        &self,
        page: &Arc<PageObject>,
    ) -> StarryResult<usize> {
        Ok(usize::from(
            !self
                .exact(page.frame().paddr().as_usize())
                .is_some_and(|node| !node.entry().is_live()),
        ))
    }

    pub(super) fn ensure_published_reservation_capacity(
        &self,
        page: &Arc<PageObject>,
    ) -> StarryResult<usize> {
        if self.exact(page.frame().paddr().as_usize()).is_some() {
            return Ok(0);
        }
        Ok(1)
    }

    fn remove(&mut self, key: usize) -> Option<Box<Node>> {
        let (root, removed) = remove_node(self.root.take(), key);
        self.root = root;
        removed
    }

    fn prune_one(&mut self, reservation: &mut CowPageIndexReservation) {
        let candidate = self
            .prune_after
            .and_then(|key| self.lower_bound(key))
            .or_else(|| self.lower_bound(0));
        let Some(node) = candidate else { return };
        let key = node.key();
        let expired = !node.entry().is_live();
        self.prune_after = key.checked_add(1);
        if expired {
            reservation.retire(
                self.remove(key)
                    .expect("selected COW node is still present"),
            );
        }
    }

    fn insert_reserved(
        &mut self,
        entry: CowPageIndexEntry,
        reservation: &mut CowPageIndexReservation,
    ) -> Result<(), (CowPageIndexInsertError, CowPageIndexEntry)> {
        let start = entry.paddr.as_usize();
        let Some(end) = entry.end().filter(|end| start < *end) else {
            return Err((
                CowPageIndexInsertError::Invalid(StarryError::BadState),
                entry,
            ));
        };
        // Validate all overlaps before touching the tree. Dead Weak owners
        // cannot become live again while the index guard excludes replacement.
        let mut overlap = self.first_overlap(start, end);
        while let Some(node) = overlap {
            if node.entry().is_live() {
                return Err((
                    CowPageIndexInsertError::Invalid(StarryError::BadState),
                    entry,
                ));
            }
            overlap = node
                .key()
                .checked_add(1)
                .and_then(|key| self.lower_bound(key))
                .filter(|next| next.key() < end);
        }
        if reservation.retired_owner.is_some() {
            return Err((CowPageIndexInsertError::StaleReservation, entry));
        }
        let mut node = if self.exact(start).is_some() {
            self.remove(start)
                .expect("expired exact node was validated")
        } else if let Some(node) = reservation.prepared.take() {
            node
        } else {
            return Err((CowPageIndexInsertError::StaleReservation, entry));
        };
        while let Some(overlap) = self.first_overlap(start, end) {
            let key = overlap.key();
            reservation.retire(self.remove(key).expect("overlap was validated"));
        }
        if let Some(retired) = node.entry.replace(entry) {
            reservation.retired_owner = Some(retired.owner);
        }
        node.height = 1;
        self.root = insert_node(self.root.take(), node);
        // Visit faster than insertion can extend the ordered tail, so a
        // monotonically growing source cannot starve expired earlier keys.
        self.prune_one(reservation);
        self.prune_one(reservation);
        Ok(())
    }

    pub(super) fn insert_pending_reserved(
        &mut self,
        page: &Arc<PageObject>,
        reservation: &mut CowPageIndexReservation,
    ) -> Result<(), CowPageIndexInsertError> {
        if page.mapping_refs() != 0 {
            return Err(CowPageIndexInsertError::Invalid(StarryError::BadState));
        }
        // The caller keeps a strong owner, so dropping a rejected duplicate
        // Arc here cannot release its allocation or physical frame.
        self.insert_reserved(CowPageIndexEntry::pending(page), reservation)
            .map_err(|(error, _entry)| error)
    }

    pub(super) fn ensure_published_reserved(
        &mut self,
        page: &Arc<PageObject>,
        reservation: &mut CowPageIndexReservation,
    ) -> Result<Option<CowPageIndexOwner>, CowPageIndexInsertError> {
        if let Some(node) = self.exact_mut(page.frame().paddr().as_usize())
            && node.entry().is_live()
        {
            if !node.entry().owner.owns(page) {
                return Err(CowPageIndexInsertError::Invalid(StarryError::BadState));
            }
            return Ok(Some(core::mem::replace(
                &mut node.entry_mut().owner,
                CowPageIndexOwner::Published(Arc::downgrade(page)),
            )));
        }
        // The caller's Arc also keeps a rejected Weak control block alive.
        self.insert_reserved(CowPageIndexEntry::published(page), reservation)
            .map(|()| None)
            .map_err(|(error, _entry)| error)
    }

    pub(super) fn get(&self, paddr: PhysAddr) -> Option<Arc<PageObject>> {
        if let Some(page) = &self.zero_page
            && page.frame().paddr() == paddr
        {
            return Some(page.clone());
        }
        let entry = self.predecessor(paddr.as_usize())?.entry();
        entry.contains(paddr)?.then(|| entry.owner.page()).flatten()
    }

    pub(super) fn publish(&mut self, page: &Arc<PageObject>) -> StarryResult<CowPageIndexOwner> {
        let entry = self
            .exact_mut(page.frame().paddr().as_usize())
            .ok_or(StarryError::BadState)?
            .entry_mut();
        if !entry.owner.owns(page) || page.mapping_refs() == 0 {
            return Err(StarryError::BadState);
        }
        Ok(core::mem::replace(
            &mut entry.owner,
            CowPageIndexOwner::Published(Arc::downgrade(page)),
        ))
    }

    pub(super) fn discard_pending(
        &mut self,
        page: &Arc<PageObject>,
    ) -> StarryResult<CowPageIndexReservation> {
        let key = page.frame().paddr().as_usize();
        let entry = self.exact(key).ok_or(StarryError::BadState)?.entry();
        if !matches!(&entry.owner, CowPageIndexOwner::Pending(current) if Arc::ptr_eq(current, page))
            || page.mapping_refs() != 0
        {
            return Err(StarryError::BadState);
        }
        let mut retired = CowPageIndexReservation::try_with_capacity(0)?;
        retired.retire(self.remove(key).expect("pending COW node was validated"));
        Ok(retired)
    }
}

fn height(link: &Link) -> usize {
    link.as_deref().map_or(0, |node| node.height)
}
fn balance(node: &Node) -> isize {
    height(&node.left) as isize - height(&node.right) as isize
}

fn rotate_left(mut root: Box<Node>) -> Box<Node> {
    let mut promoted = root
        .right
        .take()
        .expect("left rotation requires right child");
    root.right = promoted.left.take();
    root.refresh();
    promoted.left = Some(root);
    promoted.refresh();
    promoted
}

fn rotate_right(mut root: Box<Node>) -> Box<Node> {
    let mut promoted = root
        .left
        .take()
        .expect("right rotation requires left child");
    root.left = promoted.right.take();
    root.refresh();
    promoted.right = Some(root);
    promoted.refresh();
    promoted
}

fn rebalance(mut root: Box<Node>) -> Box<Node> {
    root.refresh();
    match balance(&root) {
        factor if factor > 1 => {
            if root.left.as_deref().is_some_and(|node| balance(node) < 0) {
                root.left = Some(rotate_left(root.left.take().expect("left child exists")));
            }
            rotate_right(root)
        }
        factor if factor < -1 => {
            if root.right.as_deref().is_some_and(|node| balance(node) > 0) {
                root.right = Some(rotate_right(root.right.take().expect("right child exists")));
            }
            rotate_left(root)
        }
        _ => root,
    }
}

fn insert_node(root: Link, inserted: Box<Node>) -> Link {
    let Some(mut root) = root else {
        return Some(inserted);
    };
    match inserted.key().cmp(&root.key()) {
        Ordering::Less => root.left = insert_node(root.left.take(), inserted),
        Ordering::Greater => root.right = insert_node(root.right.take(), inserted),
        Ordering::Equal => unreachable!("COW insertion removed expired overlaps"),
    }
    Some(rebalance(root))
}

fn take_min(mut root: Box<Node>) -> (Link, Box<Node>) {
    let Some(left) = root.left.take() else {
        let right = root.right.take();
        return (right, root);
    };
    let (left, removed) = take_min(left);
    root.left = left;
    (Some(rebalance(root)), removed)
}

fn remove_node(root: Link, key: usize) -> (Link, Link) {
    let Some(mut root) = root else {
        return (None, None);
    };
    match key.cmp(&root.key()) {
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

#[cfg(all(test, axtest))]
mod tests {
    use super::*;
    use crate::mm::{FrameLease, PageId};

    fn page(address: usize, size: usize) -> Arc<PageObject> {
        PageObject::new_present(
            PageId::allocate(),
            FrameLease::borrowed(address.into(), size, None).unwrap(),
        )
    }

    fn insert(
        index: &mut CowPageIndex,
        page: &Arc<PageObject>,
    ) -> Result<(), CowPageIndexInsertError> {
        let count = index.insert_reservation_capacity(page).unwrap();
        let mut reservation = CowPageIndexReservation::try_with_capacity(count).unwrap();
        index.insert_pending_reserved(page, &mut reservation)
    }

    fn expire(index: &mut CowPageIndex, page: Arc<PageObject>) {
        index
            .exact_mut(page.frame().paddr().as_usize())
            .unwrap()
            .entry_mut()
            .owner = CowPageIndexOwner::Published(Arc::downgrade(&page));
    }

    fn check_tree(link: &Link, before: Option<usize>, after: Option<usize>) -> (usize, usize) {
        let Some(node) = link else { return (0, 0) };
        assert!(before.is_none_or(|key| key < node.key()));
        assert!(after.is_none_or(|key| node.key() < key));
        let (left_height, left_count) = check_tree(&node.left, before, Some(node.key()));
        let (right_height, right_count) = check_tree(&node.right, Some(node.key()), after);
        assert!(left_height.abs_diff(right_height) <= 1);
        assert_eq!(node.height, 1 + left_height.max(right_height));
        (node.height, 1 + left_count + right_count)
    }

    #[axtest::axtest]
    fn cow_page_index_rejects_overlapping_frame_owners() {
        let mut index = CowPageIndex::new();
        let whole = page(0x400000, 2 * PAGE_SIZE_4K);
        assert!(insert(&mut index, &whole).is_ok());
        for address in [0x3ff000, 0x400000, 0x401000] {
            let conflict = page(address, 2 * PAGE_SIZE_4K);
            assert!(matches!(
                insert(&mut index, &conflict),
                Err(CowPageIndexInsertError::Invalid(StarryError::BadState))
            ));
            assert!(Arc::ptr_eq(&index.get(0x401fff.into()).unwrap(), &whole));
        }
        assert!(index.get(0x402000.into()).is_none());
        assert_eq!(check_tree(&index.root, None, None).1, 1);
    }

    #[axtest::axtest]
    fn cow_page_index_retires_expired_weak_storage_outside_the_lock() {
        let mut index = CowPageIndex::new();
        let first = page(0x500000, PAGE_SIZE_4K);
        let adjacent = page(0x501000, PAGE_SIZE_4K);
        assert!(insert(&mut index, &first).is_ok());
        assert!(insert(&mut index, &adjacent).is_ok());
        let original_node = index.exact(0x500000).unwrap() as *const Node;
        expire(&mut index, first);
        expire(&mut index, adjacent);
        let whole = page(0x500000, 2 * PAGE_SIZE_4K);
        assert_eq!(index.insert_reservation_capacity(&whole).unwrap(), 0);
        let mut reservation = CowPageIndexReservation::try_with_capacity(0).unwrap();
        assert!(
            index
                .insert_pending_reserved(&whole, &mut reservation)
                .is_ok()
        );
        assert_eq!(index.exact(0x500000).unwrap() as *const Node, original_node);
        assert!(
            reservation
                .retired_owner
                .as_ref()
                .is_some_and(|owner| !owner.is_live())
        );
        assert!(
            reservation
                .retired
                .as_ref()
                .is_some_and(|node| !node.entry().is_live())
        );
        assert!(Arc::ptr_eq(&index.get(0x501000.into()).unwrap(), &whole));
        assert_eq!(check_tree(&index.root, None, None).1, 1);
        drop(reservation);

        // Restoring an already published identity uses the same reserved path.
        let restored = page(0x600000, PAGE_SIZE_4K);
        let mut reservation = CowPageIndexReservation::try_with_capacity(
            index
                .ensure_published_reservation_capacity(&restored)
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            index.ensure_published_reserved(&restored, &mut reservation),
            Ok(None)
        ));
        assert!(Arc::ptr_eq(&index.get(0x600000.into()).unwrap(), &restored));
    }

    #[axtest::axtest]
    fn cow_page_index_revalidates_storage_before_publication() {
        let mut index = CowPageIndex::new();
        let first = page(0x700000, PAGE_SIZE_4K);
        assert!(insert(&mut index, &first).is_ok());
        expire(&mut index, first);
        let replacement = page(0x700000, PAGE_SIZE_4K);
        let mut stale = CowPageIndexReservation::try_with_capacity(
            index.insert_reservation_capacity(&replacement).unwrap(),
        )
        .unwrap();
        // A racing publisher prunes the expired node this reservation expected
        // to reuse. No candidate identity may appear before a fresh node exists.
        let racing = page(0x800000, PAGE_SIZE_4K);
        assert!(insert(&mut index, &racing).is_ok());
        assert!(index.exact(0x700000).is_none());
        assert!(matches!(
            index.insert_pending_reserved(&replacement, &mut stale),
            Err(CowPageIndexInsertError::StaleReservation)
        ));
        assert!(index.get(0x700000.into()).is_none());
        assert!(insert(&mut index, &replacement).is_ok());

        // Cross the former 256-entry growth boundary in both address orders.
        let owners: alloc::vec::Vec<_> = (0..1024)
            .map(|number| {
                let key = if number % 2 == 0 {
                    2048 - number
                } else {
                    2048 + number
                };
                page(0x1000000 + key * PAGE_SIZE_4K, PAGE_SIZE_4K)
            })
            .collect();
        for owner in &owners {
            assert!(insert(&mut index, owner).is_ok());
            check_tree(&index.root, None, None);
        }
        for owner in &owners {
            assert!(Arc::ptr_eq(
                &index.get(owner.frame().paddr()).unwrap(),
                owner
            ));
        }
        for owner in owners.iter().step_by(2) {
            let retired = index.discard_pending(owner).unwrap();
            assert!(retired.retired.as_ref().unwrap().entry().owner.owns(owner));
            assert!(index.get(owner.frame().paddr()).is_none());
            check_tree(&index.root, None, None);
        }
        for owner in owners.iter().skip(1).step_by(2) {
            assert!(Arc::ptr_eq(
                &index.get(owner.frame().paddr()).unwrap(),
                owner
            ));
        }
    }
}
