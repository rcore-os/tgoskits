//! Bounded read preparation, coalesced misses and validated publication.

mod pending;

use alloc::vec::Vec;

use axfs_ng_vfs::{FileNode, VfsError, VfsResult};
pub(super) use pending::PendingFills;
use pending::{FillAdmission, FillOwner};

use super::{CachedFile, PAGE_SIZE, PageCache};

impl CachedFile {
    pub(super) fn populate_page_window(
        &self,
        file: &FileNode,
        pn: u32,
        window_pages: usize,
    ) -> VfsResult<()> {
        let admission = {
            let _io = self.shared.io_lock.lock();
            self.shared.ensure_writeback_owner_active()?;
            if self
                .shared
                .mapping_update_in_progress
                .load(core::sync::atomic::Ordering::Acquire)
            {
                return Err(VfsError::ResourceBusy);
            }
            if self.shared.page_cache.lock().contains(&pn) {
                return Ok(());
            }
            if self.in_memory {
                self.ensure_page_locked(file, pn, false)?;
                return Ok(());
            }
            let file_len = self.shared.len();
            if u64::from(pn) >= file_len.div_ceil(PAGE_SIZE as u64) {
                return Err(VfsError::InvalidInput);
            }
            let end = (u64::from(pn)
                + window_pages.clamp(1, super::readahead::MAX_READAHEAD_PAGES) as u64)
                .min(file_len.div_ceil(PAGE_SIZE as u64))
                .min(u64::from(u32::MAX) + 1);
            self.shared
                .pending_fills
                .admit(pn, end, file_len, &self.shared.page_cache.lock())?
        };
        match admission {
            FillAdmission::Wait(fill) => fill.wait(),
            FillAdmission::Capacity => self.shared.pending_fills.wait_for_capacity(),
            FillAdmission::Load(owner) => {
                let prepared = owner.prepare(file);
                let io = self.shared.io_lock.lock();
                // Content updates invalidate the owner before modifying
                // bytes/EOF. Even an obsolete I/O error is retried against
                // current state, never published over the new contents.
                if !owner.is_valid() {
                    drop(io);
                    drop(prepared);
                    return owner.finish(Ok(()));
                }
                let result = prepared.and_then(|mut pages| {
                    // Publish the demand page last so a small retention target
                    // cannot evict it while inserting this same readahead run.
                    pages.rotate_left(1);
                    for (number, page) in pages {
                        self.insert_prepared_page_locked(file, number, page)?;
                    }
                    Ok(())
                });
                drop(io);
                owner.finish(result)
            }
        }
    }
}

impl FillOwner<'_> {
    fn prepare(&self, file: &FileNode) -> VfsResult<Vec<(u32, PageCache)>> {
        let range = self.range();
        let page_count =
            usize::try_from(range.end - range.start).map_err(|_| VfsError::InvalidInput)?;
        let run_len = page_count
            .checked_mul(PAGE_SIZE)
            .ok_or(VfsError::InvalidInput)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(run_len)
            .map_err(|_| VfsError::NoMemory)?;
        bytes.resize(run_len, 0);
        let offset = range.start * PAGE_SIZE as u64;
        let readable = self.file_len().saturating_sub(offset).min(run_len as u64) as usize;
        let read = file.read_at(&mut bytes[..readable], offset)?;
        if read > readable {
            return Err(VfsError::Io);
        }
        // The initialized staging buffer preserves EOF/short-read zero tails.
        // Each allocated page is fully overwritten, not cleared a second time.
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(page_count)
            .map_err(|_| VfsError::NoMemory)?;
        for (number, source) in range.zip(bytes.as_chunks::<PAGE_SIZE>().0) {
            let mut page = PageCache::new()?;
            page.data().copy_from_slice(source);
            pages.push((number as u32, page));
        }
        Ok(pages)
    }
}
