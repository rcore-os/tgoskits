//! Sparse shared-page ownership with lock-external path allocation.

use alloc::{boxed::Box, sync::Arc};

use super::{PageObject, SharedPageSelection, select_shared_page};
use crate::{StarryError, StarryResult};

const RADIX_BITS: usize = 4;
const RADIX_SLOTS: usize = 1 << RADIX_BITS;

enum PageNode {
    Leaf([Option<Arc<PageObject>>; RADIX_SLOTS]),
    Branch([Option<Box<PageNode>>; RADIX_SLOTS]),
}

fn digit(index: usize, level: usize) -> usize {
    (index >> (level * RADIX_BITS)) & (RADIX_SLOTS - 1)
}

/// Owns a missing suffix, prepared before taking the shared-object IRQ lock.
/// A stale suffix is returned to its caller for lock-external destruction.
pub(super) struct SharedPagePath {
    index: usize,
    level: usize,
    root: Option<Box<PageNode>>,
}

impl SharedPagePath {
    pub(super) fn prepare(index: usize, missing: Option<usize>) -> StarryResult<Self> {
        let Some(level) = missing else {
            return Ok(Self {
                index,
                level: 0,
                root: None,
            });
        };
        let mut root = Box::try_new(PageNode::Leaf([const { None }; RADIX_SLOTS]))
            .map_err(|_| StarryError::NoMemory)?;
        for parent in 1..=level {
            let mut children = [const { None }; RADIX_SLOTS];
            children[digit(index, parent)] = Some(root);
            root = Box::try_new(PageNode::Branch(children)).map_err(|_| StarryError::NoMemory)?;
        }
        Ok(Self {
            index,
            level,
            root: Some(root),
        })
    }
}

/// Nodes only grow while an object is live. Lookup and insertion are bounded
/// by the index width; neither shifts a resident-page-sized vector under IRQ
/// exclusion. The complete tree is released with its shared-object owner.
pub(super) struct SharedPageIndex {
    level: usize,
    root: Option<Box<PageNode>>,
}

impl SharedPageIndex {
    pub(super) fn new(page_count: usize) -> Self {
        let bits = usize::BITS as usize - page_count.saturating_sub(1).leading_zeros() as usize;
        Self {
            level: bits.saturating_sub(1) / RADIX_BITS,
            root: None,
        }
    }

    pub(super) fn get(&self, index: usize) -> Option<&Arc<PageObject>> {
        let mut node = self.root.as_deref()?;
        for level in (1..=self.level).rev() {
            let PageNode::Branch(children) = node else {
                unreachable!("shared-page path depth is fixed at construction");
            };
            node = children[digit(index, level)].as_deref()?;
        }
        let PageNode::Leaf(pages) = node else {
            unreachable!("shared-page terminal node must be a leaf");
        };
        pages[digit(index, 0)].as_ref()
    }

    pub(super) fn missing_level(&self, index: usize) -> Option<usize> {
        let mut link = self.root.as_deref();
        for level in (0..=self.level).rev() {
            let Some(node) = link else {
                return Some(level);
            };
            if level == 0 {
                return None;
            }
            let PageNode::Branch(children) = node else {
                unreachable!("shared-page path depth is fixed at construction");
            };
            link = children[digit(index, level)].as_deref();
        }
        None
    }

    /// Returns the candidate unchanged if a racing insertion shortened the
    /// missing suffix. Both candidate and unused path remain caller-owned.
    pub(super) fn insert(
        &mut self,
        index: usize,
        candidate: Arc<PageObject>,
        path: &mut SharedPagePath,
    ) -> Result<SharedPageSelection<PageObject>, Arc<PageObject>> {
        if path.index != index {
            return Err(candidate);
        }
        let mut link = &mut self.root;
        for level in (0..=self.level).rev() {
            if link.is_none() {
                if path.level != level || path.root.is_none() {
                    return Err(candidate);
                }
                *link = path.root.take();
            }
            match link.as_deref_mut().expect("shared-page link was populated") {
                PageNode::Leaf(pages) if level == 0 => {
                    return Ok(select_shared_page(&mut pages[digit(index, 0)], candidate));
                }
                PageNode::Branch(children) if level != 0 => {
                    link = &mut children[digit(index, level)];
                }
                _ => unreachable!("shared-page path depth is fixed at construction"),
            }
        }
        unreachable!("every shared-page path ends at a leaf")
    }
}

#[cfg(all(test, axtest))]
mod tests {
    use super::*;
    use crate::mm::{FrameLease, PageId};

    fn page(address: usize) -> Arc<PageObject> {
        PageObject::new_present(
            PageId::allocate(),
            FrameLease::borrowed(address.into(), 4096, None).unwrap(),
        )
    }

    #[axtest::axtest]
    fn prepared_shared_path_retries_after_sibling_publication() {
        let mut index = SharedPageIndex::new(256);
        let first = page(0x1000);
        let sibling = page(0x2000);
        let mut first_path = SharedPagePath::prepare(0x10, index.missing_level(0x10)).unwrap();
        let mut stale_path = SharedPagePath::prepare(0x20, index.missing_level(0x20)).unwrap();
        assert!(index.insert(0x10, first.clone(), &mut first_path).is_ok());
        let returned = index.insert(0x20, sibling.clone(), &mut stale_path).err().unwrap();
        assert!(Arc::ptr_eq(&returned, &sibling));
        assert!(index.get(0x20).is_none());
        assert!(Arc::ptr_eq(index.get(0x10).unwrap(), &first));
        drop(stale_path);
        let mut retry = SharedPagePath::prepare(0x20, index.missing_level(0x20)).unwrap();
        assert!(index.insert(0x20, returned, &mut retry).is_ok());
        assert!(Arc::ptr_eq(index.get(0x20).unwrap(), &sibling));
    }

    #[axtest::axtest]
    fn prepared_shared_path_preserves_a_racing_winner() {
        let mut index = SharedPageIndex::new(256);
        let winner = page(0x1000);
        let loser = page(0x2000);
        let mut first_path = SharedPagePath::prepare(0xff, index.missing_level(0xff)).unwrap();
        let mut stale_path = SharedPagePath::prepare(0xff, index.missing_level(0xff)).unwrap();
        assert!(index.insert(0xff, winner.clone(), &mut first_path).is_ok());
        let selected = index.insert(0xff, loser.clone(), &mut stale_path).ok().unwrap();
        assert!(Arc::ptr_eq(&selected.winner, &winner));
        assert!(Arc::ptr_eq(selected.loser.as_ref().unwrap(), &loser));
        assert!(Arc::ptr_eq(index.get(0xff).unwrap(), &winner));
    }
}
