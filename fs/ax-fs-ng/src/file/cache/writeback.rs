use alloc::{boxed::Box, vec::Vec};

use axfs_ng_vfs::{VfsError, VfsResult};

use super::{CacheMappingEvent, CacheMappingResult, CachedFileShared, PAGE_SIZE};

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

impl CachedFileShared {
    pub(super) fn writeback(&self) -> VfsResult<Vec<u32>> {
        let dirty_keys = self.begin_writeback_all_dirty()?;
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(self.len(), &dirty_keys);
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
        let result = self.writeback_page_runs(self.len(), &dirty_keys);
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
        let result = self.writeback_page_runs(self.len(), &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(data_only)?;
        Ok(())
    }

    #[cfg(feature = "vfs")]
    pub(super) fn writeback_dirty_for_global_sync(&self) -> VfsResult<()> {
        let dirty_keys = self.begin_writeback_all_dirty()?;
        if dirty_keys.is_empty() {
            return Ok(());
        }
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(self.len(), &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result
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
    fn writeback_page_runs(&self, file_len: u64, pns: &[u32]) -> VfsResult<()> {
        for batch in pns.chunks(MAX_WRITEBACK_SNAPSHOT_PAGES) {
            let snapshots = self.snapshot_dirty_pages(file_len, batch)?;
            self.writeback_snapshot_batch(&snapshots)?;
        }
        Ok(())
    }

    fn writeback_snapshot_batch(&self, snapshots: &[DirtyPageSnapshot]) -> VfsResult<()> {
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
            if let Some(current) = guard.get_mut(&page.pn)
                && current.dirty
                && current.dirty_generation == page.generation
                && !current.dirty_during_writeback
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
                let Some(page) = guard.get_mut(pn) else {
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
