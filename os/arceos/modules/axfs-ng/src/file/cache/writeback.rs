use alloc::{boxed::Box, vec::Vec};

use axfs_ng_vfs::{VfsError, VfsResult};

use super::{CachedFileShared, PAGE_SIZE, WritebackProtectListenerFn};

struct DirtyPageSnapshot {
    pn: u32,
    generation: u64,
    data: Box<[u8]>,
    len: usize,
}

impl CachedFileShared {
    pub(super) fn writeback(&self) -> VfsResult<Vec<u32>> {
        let (file_len, dirty_keys) = self.begin_writeback_all_dirty();
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(false)?;
        Ok(dirty_keys)
    }

    pub(super) fn writeback_pages(&self, pns: &[u32]) -> VfsResult<()> {
        let (file_len, dirty_keys) = self.begin_writeback_pages(pns);
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(false)?;
        Ok(())
    }

    pub(super) fn sync(&self, data_only: bool) -> VfsResult<()> {
        let (file_len, dirty_keys) = self.begin_writeback_all_dirty();
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result?;
        self.backing()?.sync(data_only)?;
        Ok(())
    }

    #[cfg(feature = "vfs")]
    pub(super) fn writeback_dirty_for_global_sync(&self) -> VfsResult<()> {
        let (file_len, dirty_keys) = self.begin_writeback_all_dirty();
        if dirty_keys.is_empty() {
            return Ok(());
        }
        self.protect_dirty_pages_before_writeback(&dirty_keys)
            .inspect_err(|_| self.cancel_writeback_tracking(&dirty_keys))?;
        let _io = self.io_lock.lock();
        let result = self.writeback_page_runs(file_len, &dirty_keys);
        self.finish_writeback_tracking(&dirty_keys);
        result
    }

    #[cfg(feature = "vfs")]
    pub(super) fn has_dirty_pages(&self) -> bool {
        self.page_cache.lock().iter().any(|(_, page)| page.dirty)
    }

    pub(super) fn protect_dirty_pages_before_writeback(&self, pns: &[u32]) -> VfsResult<()> {
        let listeners = self.writeback_protect_listeners();
        for pn in pns {
            for listener in &listeners {
                if !(listener)(*pn) {
                    return Err(VfsError::ResourceBusy);
                }
            }
        }
        Ok(())
    }

    fn begin_writeback_all_dirty(&self) -> (u64, Vec<u32>) {
        self.begin_writeback(None)
    }

    fn begin_writeback_pages(&self, pns: &[u32]) -> (u64, Vec<u32>) {
        self.begin_writeback(Some(pns))
    }

    fn begin_writeback(&self, requested: Option<&[u32]>) -> (u64, Vec<u32>) {
        let _io = self.io_lock.lock();
        let file_len = self.len();
        let mut requested_pns = requested.map(<[u32]>::to_vec);
        if let Some(pns) = requested_pns.as_mut() {
            pns.sort_unstable();
            pns.dedup();
        }
        let mut guard = self.page_cache.lock();
        let dirty_keys = guard
            .iter_mut()
            .filter_map(|(&pn, page)| {
                if !page.dirty {
                    return None;
                }
                if let Some(requested) = requested_pns.as_ref()
                    && requested.binary_search(&pn).is_err()
                {
                    return None;
                }
                let page_start = pn as u64 * PAGE_SIZE as u64;
                let len = file_len.saturating_sub(page_start).min(PAGE_SIZE as u64);
                if len == 0 {
                    return None;
                }
                page.writeback_protecting = true;
                page.dirty_during_writeback = false;
                Some(pn)
            })
            .collect();
        (file_len, dirty_keys)
    }

    fn writeback_page_runs(&self, file_len: u64, pns: &[u32]) -> VfsResult<()> {
        let mut snapshots = self.snapshot_dirty_pages(file_len, pns)?;
        snapshots.sort_by_key(|page| page.pn);

        let mut run_start = 0;
        while run_start < snapshots.len() {
            let mut run_end = run_start + 1;
            while run_end < snapshots.len()
                && snapshots[run_end].pn == snapshots[run_end - 1].pn + 1
                && snapshots[run_end - 1].len == PAGE_SIZE
            {
                run_end += 1;
            }

            self.writeback_page_run(&snapshots[run_start..run_end])?;
            run_start = run_end;
        }
        Ok(())
    }

    fn writeback_page_run(&self, snapshots: &[DirtyPageSnapshot]) -> VfsResult<()> {
        let offset = snapshots[0].pn as u64 * PAGE_SIZE as u64;
        let run_len = snapshots.iter().map(|page| page.len).sum();
        let mut data = Vec::with_capacity(run_len);
        for page in snapshots {
            data.extend_from_slice(&page.data[..page.len]);
        }
        self.backing()?.write_at(&data, offset)?;

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
        let mut guard = self.page_cache.lock();
        for pn in pns {
            let Some(page) = guard.get_mut(pn) else {
                continue;
            };
            if !page.dirty {
                continue;
            }
            let page_start = *pn as u64 * PAGE_SIZE as u64;
            let len = file_len.saturating_sub(page_start).min(PAGE_SIZE as u64) as usize;
            if len == 0 {
                continue;
            }
            snapshots.push(DirtyPageSnapshot {
                pn: *pn,
                generation: page.dirty_generation,
                data: page.data()[..len].to_vec().into_boxed_slice(),
                len,
            });
        }
        Ok(snapshots)
    }

    fn writeback_protect_listeners(&self) -> Vec<WritebackProtectListenerFn> {
        self.evict_listeners
            .lock()
            .iter()
            .map(|listener| listener.writeback_protect.clone())
            .collect()
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
