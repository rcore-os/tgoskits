//! An LRU policy must never implicitly destroy a potentially mapped frame.

use core::{
    cell::Cell,
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
};

use lru::LruCache;

use super::PageCache;

pub(super) struct CachedPages {
    pages: LruCache<u32, PageCache>,
    reclaim_target: NonZeroUsize,
    dirty_count: Cell<usize>,
}

impl CachedPages {
    pub(super) fn new(reclaim_target: NonZeroUsize) -> Self {
        Self {
            pages: LruCache::unbounded(),
            reclaim_target,
            dirty_count: Cell::new(0),
        }
    }

    pub(super) fn unbounded() -> Self {
        Self::new(NonZeroUsize::MAX)
    }

    pub(super) fn cap(&self) -> NonZeroUsize {
        self.reclaim_target
    }

    pub(super) fn len(&self) -> usize {
        self.pages.len()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub(super) fn contains(&self, page: &u32) -> bool {
        self.pages.contains(page)
    }

    pub(super) fn get(&mut self, page: &u32) -> Option<&PageCache> {
        self.pages.get(page)
    }

    pub(super) fn get_mut(&mut self, page: &u32) -> Option<CachedPageMut<'_>> {
        self.pages
            .get_mut(page)
            .map(|page| CachedPageMut::new(page, &self.dirty_count))
    }

    pub(super) fn peek_mut(&mut self, page: &u32) -> Option<CachedPageMut<'_>> {
        self.pages
            .peek_mut(page)
            .map(|page| CachedPageMut::new(page, &self.dirty_count))
    }

    pub(super) fn peek_lru(&self) -> Option<(&u32, &PageCache)> {
        self.pages.peek_lru()
    }

    pub(super) fn put(&mut self, number: u32, page: PageCache) -> Option<PageCache> {
        let dirty = usize::from(page.dirty);
        let replaced = self.pages.put(number, page);
        self.dirty_count.set(
            self.dirty_count.get() + dirty
                - usize::from(replaced.as_ref().is_some_and(|page| page.dirty)),
        );
        replaced
    }

    pub(super) fn pop(&mut self, number: &u32) -> Option<PageCache> {
        let page = self.pages.pop(number)?;
        self.dirty_count
            .set(self.dirty_count.get() - usize::from(page.dirty));
        Some(page)
    }

    pub(super) fn pop_lru(&mut self) -> Option<(u32, PageCache)> {
        let (number, page) = self.pages.pop_lru()?;
        self.dirty_count
            .set(self.dirty_count.get() - usize::from(page.dirty));
        Some((number, page))
    }

    pub(super) fn iter(&self) -> impl DoubleEndedIterator<Item = (&u32, &PageCache)> {
        self.pages.iter()
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = (&u32, CachedPageMut<'_>)> {
        self.pages
            .iter_mut()
            .map(|(number, page)| (number, CachedPageMut::new(page, &self.dirty_count)))
    }

    pub(super) fn dirty_count(&self) -> usize {
        self.dirty_count.get()
    }

    pub(super) fn set_reclaim_target(&mut self, target: NonZeroUsize) {
        // Unlike LruCache::resize, changing the target must not implicitly
        // release frames: eviction and truncate own mapping invalidation.
        self.reclaim_target = target;
    }
}

// Mutable access never escapes this guard, so all dirty transitions (including
// rollback and redirty during writeback) are accounted before releasing the
// cache index. Cell allows disjoint iterator entries to share the counter;
// the enclosing page-cache mutex still serializes every access.
pub(super) struct CachedPageMut<'a> {
    page: &'a mut PageCache,
    dirty_count: &'a Cell<usize>,
    was_dirty: bool,
}

impl<'a> CachedPageMut<'a> {
    fn new(page: &'a mut PageCache, dirty_count: &'a Cell<usize>) -> Self {
        let was_dirty = page.dirty;
        Self {
            page,
            dirty_count,
            was_dirty,
        }
    }
}

impl Deref for CachedPageMut<'_> {
    type Target = PageCache;

    fn deref(&self) -> &Self::Target {
        self.page
    }
}

impl DerefMut for CachedPageMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.page
    }
}

impl Drop for CachedPageMut<'_> {
    fn drop(&mut self) {
        self.dirty_count.set(
            self.dirty_count.get() + usize::from(self.page.dirty) - usize::from(self.was_dirty),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os::memory::test_support::with_test_page_provider;

    #[test]
    fn dirty_accounting_survives_writeback_and_membership_changes() {
        with_test_page_provider(true, |_| {
            let mut pages = CachedPages::unbounded();
            for number in 0..3 {
                let mut page = PageCache::new().unwrap();
                if number != 0 {
                    page.mark_dirty();
                }
                assert!(pages.put(number, page).is_none());
            }
            assert_eq!(pages.dirty_count(), 2);

            // Repeated dirties do not add pages; failed and stale writebacks
            // cannot clear the dirty state of newer content.
            {
                let mut page = pages.get_mut(&0).unwrap();
                page.mark_dirty();
                page.mark_dirty();
                page.begin_writeback().unwrap();
                page.finish_writeback();
            }
            assert_eq!(pages.dirty_count(), 3);
            {
                let mut page = pages.peek_mut(&0).unwrap();
                page.begin_writeback().unwrap();
                let generation = page.dirty_generation;
                page.mark_dirty();
                page.complete_writeback(generation);
                page.finish_writeback();
            }
            assert_eq!(pages.dirty_count(), 3);
            {
                let mut page = pages.get_mut(&0).unwrap();
                page.begin_writeback().unwrap();
                let generation = page.dirty_generation;
                page.complete_writeback(generation);
                page.finish_writeback();
            }
            assert_eq!(pages.dirty_count(), 2);

            // Replacement transfers ownership of a dirty entry out of the
            // index, and reinsertion must account for that state again.
            let replaced = pages.put(1, PageCache::new().unwrap()).unwrap();
            assert_eq!(pages.dirty_count(), 1);
            assert!(pages.put(3, replaced).is_none());
            assert_eq!(pages.dirty_count(), 2);
            pages.pop(&2).unwrap().retire_invalidated();
            assert_eq!(pages.dirty_count(), 1);

            // Iterators can hold disjoint guards at once and stop early.
            let mut entries = pages.iter_mut();
            let (_, mut first) = entries.next().unwrap();
            let (_, mut second) = entries.next().unwrap();
            first.dirty = false;
            second.dirty = false;
            drop((first, second, entries));
            assert_eq!(pages.dirty_count(), 0);
            for (_, mut page) in pages.iter_mut() {
                page.mark_dirty();
            }
            assert_eq!(pages.dirty_count(), pages.len());
            while let Some((_, page)) = pages.pop_lru() {
                page.retire_invalidated();
            }
            assert_eq!(pages.dirty_count(), 0);
        });
    }
}
