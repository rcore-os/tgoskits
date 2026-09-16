#[cfg(all(feature = "ext4", feature = "vfs"))]
use alloc::sync::Arc;
use alloc::{boxed::Box, vec::Vec};

use axfs_ng_vfs::{VfsError, VfsResult};

#[cfg(feature = "vfs")]
use super::DIRTY_PAGE_HARD_WATERMARK;
use super::{
    CacheMappingEvent, CacheMappingResult, CachedFileShared, DIRTY_PAGE_BACKGROUND_WATERMARK,
    DIRTY_PAGE_LOW_WATERMARK, PAGE_SIZE,
};

/// Upper bound for one detached writeback snapshot batch.
///
/// Linux writeback submits bounded folio/bio batches and never concatenates an
/// arbitrarily long dirty extent into a second full-size heap buffer.  The VFS
/// backing interface is not scatter/gather-aware yet, so this implementation
/// writes each stable page snapshot separately while bounding the number of
/// snapshots retained across I/O.
const MAX_WRITEBACK_SNAPSHOT_PAGES: usize = 16;

struct DirtyPageSnapshot {
    pn: u32,
    generation: u64,
    data: Box<[u8]>,
    len: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WritebackCompletionMode {
    /// Preserve a page redirtied while an explicit writeback was protecting
    /// mappings without holding `io_lock`.
    Tracked,
    /// The buffered writer owns `io_lock`, so a matching snapshot is the
    /// newest page generation even if another writeback round tracks it.
    WriterOwned,
}

impl CachedFileShared {
    pub(super) fn balance_dirty_pages_locked(&self) -> VfsResult<()> {
        let dirty_count = self.dirty_page_count();
        if dirty_count < DIRTY_PAGE_BACKGROUND_WATERMARK {
            return Ok(());
        }

        #[cfg(feature = "vfs")]
        {
            self.request_background_writeback();
            let worker_running = super::writeback_worker::request_background_writeback();
            let mapped = self.has_mapping_endpoint();
            if !worker_running {
                self.take_background_writeback_request();
                if mapped {
                    return Ok(());
                }
            } else if dirty_count < DIRTY_PAGE_HARD_WATERMARK || mapped {
                return Ok(());
            }
        }

        self.writeback_dirty_to_low_watermark_locked()
    }

    pub(super) fn dirty_page_count(&self) -> usize {
        self.page_cache
            .lock()
            .iter()
            .filter(|(_, page)| page.dirty)
            .count()
    }

    #[cfg(feature = "vfs")]
    pub(super) fn writeback_dirty_for_background(&self) -> VfsResult<()> {
        let dirty_keys = self.begin_writeback_to_low_watermark()?;
        if dirty_keys.is_empty() {
            return Ok(());
        }
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        if self.retired.load(core::sync::atomic::Ordering::Acquire)
            || self.unlinked.load(core::sync::atomic::Ordering::Acquire)
        {
            self.finish_writeback_tracking(&dirty_keys);
            return Ok(());
        }
        let result =
            self.writeback_page_runs(self.len(), &dirty_keys, WritebackCompletionMode::Tracked);
        self.finish_writeback_tracking(&dirty_keys);
        result
    }

    #[cfg(feature = "vfs")]
    pub(super) fn writeback_dirty_for_periodic(&self) -> VfsResult<()> {
        let dirty_keys = self.begin_writeback_all_dirty()?;
        if dirty_keys.is_empty() {
            return Ok(());
        }
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        if self.retired.load(core::sync::atomic::Ordering::Acquire)
            || self.unlinked.load(core::sync::atomic::Ordering::Acquire)
        {
            self.finish_writeback_tracking(&dirty_keys);
            return Ok(());
        }
        let result =
            self.writeback_page_runs(self.len(), &dirty_keys, WritebackCompletionMode::Tracked);
        self.finish_writeback_tracking(&dirty_keys);
        result
    }

    /// Writes older dirty pages down to the low watermark while buffered I/O
    /// owns `io_lock`. Files with a live mapping endpoint use the existing
    /// reverse-mapping-aware writeback path instead.
    pub(super) fn writeback_dirty_to_low_watermark_locked(&self) -> VfsResult<()> {
        if self.has_mapping_endpoint() {
            return Ok(());
        }

        let dirty_count = self.dirty_page_count();
        if dirty_count < DIRTY_PAGE_BACKGROUND_WATERMARK {
            return Ok(());
        }

        let writeback_count = dirty_count - DIRTY_PAGE_LOW_WATERMARK;
        let mut dirty_keys = Vec::new();
        dirty_keys
            .try_reserve_exact(writeback_count)
            .map_err(|_| VfsError::NoMemory)?;
        {
            let cache = self.page_cache.lock();
            dirty_keys.extend(
                cache
                    .iter()
                    .rev()
                    .filter_map(|(&pn, page)| page.dirty.then_some(pn))
                    .take(writeback_count),
            );
        }
        if dirty_keys.len() != writeback_count {
            return Err(VfsError::BadState);
        }

        dirty_keys.sort_unstable();
        self.writeback_page_runs(
            self.len(),
            &dirty_keys,
            WritebackCompletionMode::WriterOwned,
        )
    }

    /// Writes the least-recently-used dirty page while buffered I/O owns
    /// `io_lock`, making room for the next cache insertion.
    pub(super) fn writeback_lru_for_capacity_locked(&self) -> VfsResult<()> {
        let page_number = {
            let cache = self.page_cache.lock();
            let Some((&page_number, _)) = cache.peek_lru() else {
                return Err(VfsError::BadState);
            };
            let page = cache.peek(&page_number).ok_or(VfsError::BadState)?;
            if page.pins != 0 {
                return Err(VfsError::ResourceBusy);
            }
            if !page.dirty {
                return Ok(());
            }
            page_number
        };

        self.writeback_page_runs(
            self.len(),
            core::slice::from_ref(&page_number),
            WritebackCompletionMode::WriterOwned,
        )
    }

    pub(super) fn writeback(&self) -> VfsResult<Vec<u32>> {
        let dirty_keys = self.begin_writeback_all_dirty()?;
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result =
            self.writeback_page_runs(self.len(), &dirty_keys, WritebackCompletionMode::Tracked);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(false)?;
        Ok(dirty_keys)
    }

    pub(super) fn writeback_pages(&self, pns: &[u32]) -> VfsResult<()> {
        let dirty_keys = self.begin_writeback_pages(pns)?;
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result =
            self.writeback_page_runs(self.len(), &dirty_keys, WritebackCompletionMode::Tracked);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(false)?;
        Ok(())
    }

    pub(super) fn sync(&self, data_only: bool) -> VfsResult<()> {
        let dirty_keys = self.begin_writeback_all_dirty()?;
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result =
            self.writeback_page_runs(self.len(), &dirty_keys, WritebackCompletionMode::Tracked);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(data_only)?;
        Ok(())
    }

    #[cfg(any(feature = "vfs", feature = "ext4"))]
    pub(super) fn writeback_dirty_for_global_sync(&self) -> VfsResult<()> {
        let dirty_keys = self.begin_writeback_all_dirty()?;
        if dirty_keys.is_empty() {
            return Ok(());
        }
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        #[cfg(feature = "vfs")]
        if self.retired.load(core::sync::atomic::Ordering::Acquire)
            || self.unlinked.load(core::sync::atomic::Ordering::Acquire)
        {
            self.finish_writeback_tracking(&dirty_keys);
            return Ok(());
        }
        let result =
            self.writeback_page_runs(self.len(), &dirty_keys, WritebackCompletionMode::Tracked);
        self.finish_writeback_tracking(&dirty_keys);
        result
    }

    /// Stops registry writeback, waits for in-flight file I/O, and performs
    /// the final dirty writeback before dropping registry ownership.
    #[cfg(all(feature = "ext4", feature = "vfs"))]
    pub(super) fn retire_from_writeback_registry(self: &Arc<Self>) -> VfsResult<()> {
        #[cfg(test)]
        self.notify_retirement_lock_attempt();

        {
            let _io = self.io_lock.lock();
            self.retired
                .store(true, core::sync::atomic::Ordering::Release);
        }

        let dirty_keys = match self.begin_writeback_all_dirty() {
            Ok(dirty_keys) => dirty_keys,
            Err(error) => {
                self.cancel_retirement();
                return Err(error);
            }
        };
        if let Err(error) = self.protect_dirty_pages_before_writeback(&dirty_keys) {
            self.cancel_writeback_tracking(&dirty_keys);
            self.cancel_retirement();
            return Err(error);
        }

        let _io = self.io_lock.lock();
        let result =
            self.writeback_page_runs(self.len(), &dirty_keys, WritebackCompletionMode::Tracked);
        self.finish_writeback_tracking(&dirty_keys);

        if let Err(error) = result {
            self.retired
                .store(false, core::sync::atomic::Ordering::Release);
            return Err(error);
        }

        super::reclaim::release_cached_file(self);
        Ok(())
    }

    #[cfg(all(feature = "ext4", feature = "vfs"))]
    fn cancel_retirement(&self) {
        let _io = self.io_lock.lock();
        self.retired
            .store(false, core::sync::atomic::Ordering::Release);
    }

    #[cfg(feature = "vfs")]
    pub(super) fn has_dirty_pages(&self) -> bool {
        self.page_cache.lock().iter().any(|(_, page)| page.dirty)
    }

    pub(super) fn protect_dirty_pages_before_writeback(&self, pns: &[u32]) -> VfsResult<()> {
        for pn in pns {
            let Some(paddr) = ({
                let mut cache = self.page_cache.lock();
                cache.get_mut(pn).map(|page| page.paddr()).transpose()?
            }) else {
                continue;
            };
            let event = CacheMappingEvent::WritebackProtect(self.cache_page_identity(*pn, paddr));
            match self.publish_mapping_event(event) {
                CacheMappingResult::Protected => {}
                CacheMappingResult::Busy | CacheMappingResult::Quarantined => {
                    return Err(VfsError::ResourceBusy);
                }
                CacheMappingResult::Retired | CacheMappingResult::Failed => {
                    return Err(VfsError::BadState);
                }
            }
        }
        Ok(())
    }

    fn begin_writeback_all_dirty(&self) -> VfsResult<Vec<u32>> {
        self.begin_writeback(None)
    }

    #[cfg(feature = "vfs")]
    fn begin_writeback_to_low_watermark(&self) -> VfsResult<Vec<u32>> {
        let _io = self.io_lock.lock();
        let dirty_count = self.dirty_page_count();
        if dirty_count < DIRTY_PAGE_BACKGROUND_WATERMARK {
            return Ok(Vec::new());
        }

        let writeback_count = dirty_count - DIRTY_PAGE_LOW_WATERMARK;
        let mut dirty_keys = Vec::new();
        dirty_keys
            .try_reserve_exact(writeback_count)
            .map_err(|_| VfsError::NoMemory)?;
        {
            let cache = self.page_cache.lock();
            dirty_keys.extend(
                cache
                    .iter()
                    .rev()
                    .filter_map(|(&pn, page)| page.dirty.then_some(pn))
                    .take(writeback_count),
            );
        }
        if dirty_keys.len() != writeback_count {
            return Err(VfsError::BadState);
        }
        dirty_keys.sort_unstable();
        let mut cache = self.page_cache.lock();
        for pn in &dirty_keys {
            let page = cache.peek_mut(pn).ok_or(VfsError::BadState)?;
            if !page.dirty {
                return Err(VfsError::BadState);
            }
            page.writeback_protecting = true;
            page.dirty_during_writeback = false;
        }
        drop(cache);
        Ok(dirty_keys)
    }

    fn begin_writeback_pages(&self, pns: &[u32]) -> VfsResult<Vec<u32>> {
        self.begin_writeback(Some(pns))
    }

    fn begin_writeback(&self, requested: Option<&[u32]>) -> VfsResult<Vec<u32>> {
        let _io = self.io_lock.lock();
        let file_len = self.len();
        let mut requested_pns = if let Some(requested) = requested {
            let mut copy = Vec::new();
            copy.try_reserve_exact(requested.len())
                .map_err(|_| VfsError::NoMemory)?;
            copy.extend_from_slice(requested);
            Some(copy)
        } else {
            None
        };
        if let Some(pns) = requested_pns.as_mut() {
            pns.sort_unstable();
            pns.dedup();
        }
        let mut dirty_keys = Vec::new();
        loop {
            dirty_keys.clear();
            let required = self.page_cache.lock().len();
            if dirty_keys.capacity() < required {
                dirty_keys
                    .try_reserve_exact(required)
                    .map_err(|_| VfsError::NoMemory)?;
            }

            let mut guard = self.page_cache.lock();
            if guard.len() > dirty_keys.capacity() {
                continue;
            }
            for (&pn, page) in guard.iter_mut() {
                if !page.dirty {
                    continue;
                }
                if let Some(requested) = requested_pns.as_ref()
                    && requested.binary_search(&pn).is_err()
                {
                    continue;
                }
                let page_start = pn as u64 * PAGE_SIZE as u64;
                let len = file_len.saturating_sub(page_start).min(PAGE_SIZE as u64);
                if len == 0 {
                    continue;
                }
                page.writeback_protecting = true;
                page.dirty_during_writeback = false;
                dirty_keys.push(pn);
            }
            break;
        }
        dirty_keys.sort_unstable();
        Ok(dirty_keys)
    }

    // The caller samples EOF only after reacquiring io_lock: mapping
    // protection runs lock-external and may race a committed truncate/write.
    fn writeback_page_runs(
        &self,
        file_len: u64,
        pns: &[u32],
        completion_mode: WritebackCompletionMode,
    ) -> VfsResult<()> {
        for batch in pns.chunks(MAX_WRITEBACK_SNAPSHOT_PAGES) {
            let snapshots = self.snapshot_dirty_pages(file_len, batch)?;
            self.writeback_snapshot_batch(&snapshots, completion_mode)?;
        }
        Ok(())
    }

    fn writeback_snapshot_batch(
        &self,
        snapshots: &[DirtyPageSnapshot],
        completion_mode: WritebackCompletionMode,
    ) -> VfsResult<()> {
        let backing = self.backing()?;
        for page in snapshots {
            let offset = page.pn as u64 * PAGE_SIZE as u64;
            let mut written = 0;
            while written < page.len {
                let count =
                    backing.write_at(&page.data[written..page.len], offset + written as u64)?;
                if count == 0 || count > page.len - written {
                    return Err(VfsError::Io);
                }
                written += count;
            }
        }

        let mut guard = self.page_cache.lock();
        for page in snapshots {
            if let Some(current) = guard.peek_mut(&page.pn)
                && current.dirty
                && current.dirty_generation == page.generation
                && (completion_mode == WritebackCompletionMode::WriterOwned
                    || !current.dirty_during_writeback)
            {
                current.dirty = false;
            }
        }
        Ok(())
    }

    fn snapshot_dirty_pages(
        &self,
        file_len: u64,
        pns: &[u32],
    ) -> VfsResult<Vec<DirtyPageSnapshot>> {
        let mut snapshots = Vec::new();
        snapshots
            .try_reserve_exact(pns.len())
            .map_err(|_| VfsError::NoMemory)?;
        for pn in pns {
            let page_start = *pn as u64 * PAGE_SIZE as u64;
            let len = file_len.saturating_sub(page_start).min(PAGE_SIZE as u64) as usize;
            if len == 0 {
                continue;
            }
            let mut data = Vec::new();
            data.try_reserve_exact(len)
                .map_err(|_| VfsError::NoMemory)?;
            let generation = {
                let mut guard = self.page_cache.lock();
                let Some(page) = guard.peek_mut(pn) else {
                    continue;
                };
                if !page.dirty {
                    continue;
                }
                data.extend_from_slice(&page.data()[..len]);
                page.dirty_generation
            };
            if data.len() != len {
                return Err(VfsError::BadState);
            }
            snapshots.push(DirtyPageSnapshot {
                pn: *pn,
                generation,
                data: data.into_boxed_slice(),
                len,
            });
        }
        Ok(snapshots)
    }

    fn cancel_writeback_tracking(&self, pns: &[u32]) {
        let _io = self.io_lock.lock();
        self.finish_writeback_tracking(pns);
    }

    fn finish_writeback_tracking(&self, pns: &[u32]) {
        let mut guard = self.page_cache.lock();
        for pn in pns {
            if let Some(page) = guard.get_mut(pn) {
                page.writeback_protecting = false;
                page.dirty_during_writeback = false;
            }
        }
    }
}
