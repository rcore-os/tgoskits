//! An LRU policy must never implicitly destroy a potentially mapped frame.

use core::num::NonZeroUsize;

use lru::LruCache;

use super::PageCache;

pub(super) struct CachedPages {
    pages: LruCache<u32, PageCache>,
    reclaim_target: NonZeroUsize,
}

impl CachedPages {
    pub(super) fn new(reclaim_target: NonZeroUsize) -> Self {
        Self {
            pages: LruCache::unbounded(),
            reclaim_target,
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

    pub(super) fn get_mut(&mut self, page: &u32) -> Option<&mut PageCache> {
        self.pages.get_mut(page)
    }

    pub(super) fn peek_mut(&mut self, page: &u32) -> Option<&mut PageCache> {
        self.pages.peek_mut(page)
    }

    pub(super) fn peek_lru(&self) -> Option<(&u32, &PageCache)> {
        self.pages.peek_lru()
    }

    pub(super) fn put(&mut self, number: u32, page: PageCache) -> Option<PageCache> {
        self.pages.put(number, page)
    }

    pub(super) fn pop(&mut self, number: &u32) -> Option<PageCache> {
        self.pages.pop(number)
    }

    pub(super) fn pop_lru(&mut self) -> Option<(u32, PageCache)> {
        self.pages.pop_lru()
    }

    pub(super) fn iter(&self) -> impl DoubleEndedIterator<Item = (&u32, &PageCache)> {
        self.pages.iter()
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = (&u32, &mut PageCache)> {
        self.pages.iter_mut()
    }

    pub(super) fn set_reclaim_target(&mut self, target: NonZeroUsize) {
        // Unlike LruCache::resize, changing the target must not implicitly
        // release frames: eviction and truncate own mapping invalidation.
        self.reclaim_target = target;
    }
}
