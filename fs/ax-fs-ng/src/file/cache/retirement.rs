//! Truncated frames remain owned until all mappings acknowledge invalidation.

use alloc::vec::Vec;

use axfs_ng_vfs::{VfsError, VfsResult};

use super::{CachedFileShared, PageCache};

impl CachedFileShared {
    /// Call with the writeback owner, but without cached-I/O or page locks.
    /// Each failed frame is restored to retirement, never to the live index.
    pub(super) fn finish_retired_pages(&self) -> VfsResult<()> {
        if self.retired_pages.lock().is_empty() {
            return Ok(());
        }
        let listeners = self
            .evict_listeners
            .lock()
            .iter()
            .map(|entry| entry.discard.clone())
            .collect::<Vec<_>>();
        let pages = core::mem::take(&mut *self.retired_pages.lock());
        let mut rejected = Vec::new();
        for (number, page) in pages {
            if !listeners.iter().all(|listener| listener(number, &page)) {
                rejected.push((number, page));
            }
        }
        if rejected.is_empty() {
            Ok(())
        } else {
            self.retired_pages.lock().extend(rejected);
            Err(VfsError::ResourceBusy)
        }
    }

    /// Retry invalidations under the writeback owner. Ordinary writes also
    /// hold mutation exclusion so they cannot regrow past a pending truncate;
    /// sync callers only complete retirement. Faults acquire neither owner.
    pub(super) fn retry_retired_pages(&self) -> VfsResult<()> {
        if self.retired_pages.lock().is_empty() {
            return Ok(());
        }
        let _writeback = self.writeback_lock.lock();
        self.finish_retired_pages()
    }

    pub(super) fn retain_discarded_pages(&self, pages: Vec<(u32, PageCache)>) {
        self.retired_pages.lock().extend(pages);
    }
}
