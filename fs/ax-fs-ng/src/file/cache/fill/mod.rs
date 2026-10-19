//! Bounded read preparation, coalesced misses and validated publication.

mod pending;

use alloc::vec::Vec;

use axfs_ng_vfs::{FileNode, VfsError, VfsResult};
pub(super) use pending::PendingFills;
use pending::{FillAdmission, FillOwner};

use super::{CacheMappingEvent, CacheMappingResult, CachedFile, PAGE_SIZE, PageCache};

impl CachedFile {
    pub(super) fn populate_page_window(
        &self,
        file: &FileNode,
        pn: u32,
        window_pages: usize,
    ) -> VfsResult<()> {
        self.fill_page_window(file, pn, window_pages, FillPurpose::Demand)
            .map(|_| ())
    }

    pub(super) fn prefetch_page_window(&self, pn: u32, window_pages: usize) -> VfsResult<bool> {
        let file = self.inner.entry().as_file()?;
        let end = (u64::from(pn) + window_pages.min(super::readahead::MAX_READAHEAD_PAGES) as u64)
            .min(self.shared.len().div_ceil(PAGE_SIZE as u64))
            .min(u64::from(u32::MAX) + 1);
        // Another handle may have populated any prefix or interior page.
        // Walk the bounded window instead of treating its first cache hit as
        // successful prefetch of every remaining page.
        for number in u64::from(pn)..end {
            if !self.fill_page_window(
                file,
                number as u32,
                (end - number) as usize,
                FillPurpose::Prefetch,
            )? {
                return Ok(false);
            }
        }
        Ok(end > u64::from(pn))
    }

    fn fill_page_window(
        &self,
        file: &FileNode,
        pn: u32,
        window_pages: usize,
        purpose: FillPurpose,
    ) -> VfsResult<bool> {
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
                return Ok(true);
            }
            if self.in_memory {
                self.ensure_page_locked(file, pn, false)?;
                return Ok(true);
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
            FillAdmission::Wait(_) | FillAdmission::Capacity
                if purpose == FillPurpose::Prefetch =>
            {
                Ok(false)
            }
            FillAdmission::Wait(fill) => fill.wait().map(|_| false),
            FillAdmission::Capacity => self.shared.pending_fills.wait_for_capacity().map(|_| false),
            FillAdmission::Load(owner) => {
                let range = owner.range();
                let prepared = owner.prepare(file);
                let io = self.shared.io_lock.lock();
                // Content updates invalidate the owner before modifying
                // bytes/EOF. Even an obsolete I/O error is retried against
                // current state, never published over the new contents.
                if !owner.is_valid() {
                    drop(io);
                    drop(prepared);
                    return owner.finish(Ok(())).map(|_| false);
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
                if result.is_ok() && purpose == FillPurpose::Demand {
                    self.readahead.lock().loaded(range);
                }
                if purpose == FillPurpose::Prefetch {
                    // A demand that joined speculative I/O retries on failure.
                    // It must not inherit the speculative owner's transient error.
                    owner.finish(Ok(()))?;
                    result.map(|_| true)
                } else {
                    owner.finish(result).map(|_| true)
                }
            }
        }
    }

    /// Retires clean cache owners after a buffered read has copied its demand
    /// page. A live endpoint prevents the insertion path from detaching pages;
    /// its typed callback invalidates any PTE before a frame is freed.
    pub(super) fn trim_clean_pages_after_read(&self) {
        let over_target = {
            let cache = self.shared.page_cache.lock();
            cache.len() > cache.cap().get()
        };
        if !over_target || !self.shared.has_mapping_endpoint() {
            return;
        }

        let _mapping_update = match self.begin_mapping_reclaim() {
            Ok(guard) => guard,
            Err(VfsError::ResourceBusy) => return,
            Err(error) => {
                log::warn!("clean mapped cache trim deferred: {error:?}");
                return;
            }
        };

        const BATCH: usize = 64;
        let mut candidates = [0u32; BATCH];
        let mut count = 0;
        {
            let cache = self.shared.page_cache.lock();
            if cache.len() <= cache.cap().get() {
                return;
            }
            for (&number, page) in cache.iter().rev() {
                if !page.dirty && page.pins == 0 {
                    candidates[count] = number;
                    count += 1;
                    if count == BATCH {
                        break;
                    }
                }
            }
        }

        for number in candidates.into_iter().take(count) {
            let io = self.shared.io_lock.lock();
            let candidate = {
                let mut cache = self.shared.page_cache.lock();
                if cache.len() <= cache.cap().get() {
                    return;
                }
                match cache.peek_mut(&number) {
                    Some(page) if !page.dirty && page.pins == 0 => cache.pop(&number),
                    _ => None,
                }
            };
            drop(io);
            let Some(page) = candidate else {
                continue;
            };

            let result = match page.paddr() {
                Ok(paddr) => self.shared.publish_mapping_event(CacheMappingEvent::Evict(
                    self.cache_page_identity(number, paddr),
                )),
                Err(error) => {
                    log::warn!("clean mapped cache trim deferred: {error:?}");
                    CacheMappingResult::Failed
                }
            };
            match result {
                CacheMappingResult::Retired => {}
                CacheMappingResult::Busy | CacheMappingResult::Quarantined => {
                    let replaced = self.shared.page_cache.lock().put(number, page);
                    drop(replaced);
                }
                CacheMappingResult::Protected | CacheMappingResult::Failed => {
                    let replaced = self.shared.page_cache.lock().put(number, page);
                    drop(replaced);
                    log::warn!("clean mapped cache trim failed for page {number}: {result:?}");
                    return;
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FillPurpose {
    Demand,
    Prefetch,
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
