//! Stable cache-hit reads while another page is being loaded from storage.

use core::{io::BorrowedCursor, ops::Range, sync::atomic::Ordering};

use super::CachedFileShared;
use crate::os::sync::MutexGuard;

/// Keeps tentative content changes hidden until commit or rollback completes.
pub(super) struct CacheUpdateGuard<'a> {
    shared: &'a CachedFileShared,
    io: Option<MutexGuard<'a, ()>>,
}

impl CacheUpdateGuard<'_> {
    /// Publishes completed bytes before a potentially blocking writeback step.
    /// The caller retains mapping-layout serialization throughout this phase.
    pub(super) fn without_io<R>(&mut self, operation: impl FnOnce() -> R) -> R {
        self.shared.updating.store(false, Ordering::Release);
        drop(self.io.take());
        let result = operation();
        self.io = Some(self.shared.io_lock.lock());
        self.shared.pending_fills.invalidate();
        self.shared.updating.store(true, Ordering::Release);
        result
    }
}

impl CachedFileShared {
    pub(super) fn lock_for_update(&self) -> CacheUpdateGuard<'_> {
        let io = self.io_lock.lock();
        self.pending_fills.invalidate();
        // Only this lock's owner can publish or clear the update state. Every
        // content mutation subsequently takes page_cache, in that order.
        self.updating.store(true, Ordering::Release);
        CacheUpdateGuard {
            shared: self,
            io: Some(io),
        }
    }

    /// Copies only into resident kernel storage, never an arbitrary Writer.
    pub(super) fn try_copy_cached_page(
        &self,
        page_number: u32,
        source: Range<usize>,
        mut dst: BorrowedCursor<'_, u8>,
    ) -> Option<usize> {
        // A busy cache index does not imply a missing page. Wait for the index
        // before deciding whether to take io_lock, which may be held by an
        // unrelated backing read. Drop this guard before every slow fallback.
        let mut cache = self.page_cache.lock();
        if self.updating.load(Ordering::Acquire)
            || self.mapping_update_in_progress.load(Ordering::Acquire)
        {
            return None;
        }
        let remaining = self
            .len()
            .saturating_sub(u64::from(page_number) * super::PAGE_SIZE as u64);
        let end = source
            .end
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        if end <= source.start {
            return Some(0);
        }
        let page = cache.get_mut(&page_number)?;
        // The cache lock pins the page and serializes content access. If a
        // writer publishes after our check, it cannot mutate until this copy
        // ends. Otherwise the update flag hides its tentative changes, even
        // when the writer drops page_cache while waiting for backing I/O.
        dst.append(&page.data()[source.start..end]);
        Some(end - source.start)
    }
}

impl Drop for CacheUpdateGuard<'_> {
    fn drop(&mut self) {
        // Publish stable bytes before dropping io, including all error paths.
        self.shared.updating.store(false, Ordering::Release);
    }
}
