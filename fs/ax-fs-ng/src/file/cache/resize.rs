use alloc::vec::Vec;

use axfs_ng_vfs::{FileNode, VfsError, VfsResult};

use super::{CachedFile, PAGE_SIZE, PageCache};

struct PreparedPageWrite {
    page_number: u32,
    page_offset: usize,
    generation: u64,
    was_dirty: bool,
    original_data: Vec<u8>,
    zeroed_data: Vec<u8>,
}

impl CachedFile {
    pub(super) fn zero_partial_page_locked(
        &self,
        file: &FileNode,
        page_number: u32,
        zero_start: usize,
        zero_end: usize,
    ) -> VfsResult<()> {
        let mut guard = self.shared.page_cache.lock();
        let page = self.page_or_insert(file, &mut guard, page_number, true)?.0;
        page.data()[zero_start..zero_end].fill(0);
        if !self.in_memory {
            page.mark_dirty();
        }
        Ok(())
    }

    fn prepare_zero_write_locked(
        &self,
        file: &FileNode,
        page_number: u32,
        zero_start: usize,
        zero_end: usize,
        persist_end: usize,
    ) -> VfsResult<PreparedPageWrite> {
        let mut guard = self.shared.page_cache.lock();
        let page = self.page_or_insert(file, &mut guard, page_number, true)?.0;
        let was_dirty = page.dirty;
        let original_data = page.data()[zero_start..zero_end].to_vec();
        page.data()[zero_start..zero_end].fill(0);
        if !self.in_memory {
            page.mark_dirty();
        }
        let generation = page.dirty_generation;
        let zeroed_data = page.data()[zero_start..persist_end].to_vec();
        Ok(PreparedPageWrite {
            page_number,
            page_offset: zero_start,
            generation,
            was_dirty,
            original_data,
            zeroed_data,
        })
    }

    fn prepared_offset(prepared: &PreparedPageWrite) -> u64 {
        u64::from(prepared.page_number) * PAGE_SIZE as u64 + prepared.page_offset as u64
    }

    fn persist_prepared_page(
        &self,
        file: &FileNode,
        prepared: &PreparedPageWrite,
    ) -> VfsResult<()> {
        if self.in_memory {
            return Ok(());
        }
        if file.write_at(&prepared.zeroed_data, Self::prepared_offset(prepared))?
            != prepared.zeroed_data.len()
        {
            return Err(VfsError::Io);
        }
        Ok(())
    }

    fn finish_prepared_page(&self, prepared: &PreparedPageWrite) {
        if self.in_memory || prepared.was_dirty {
            return;
        }
        let mut guard = self.shared.page_cache.lock();
        if let Some(page) = guard.get_mut(&prepared.page_number)
            && page.dirty
            && page.dirty_generation == prepared.generation
        {
            page.dirty = false;
        }
    }

    fn restore_prepared_cache(&self, prepared: &PreparedPageWrite, backing_restored: bool) {
        let mut guard = self.shared.page_cache.lock();
        if let Some(page) = guard.get_mut(&prepared.page_number)
            && page.dirty_generation == prepared.generation
        {
            let end = prepared.page_offset + prepared.original_data.len();
            page.data()[prepared.page_offset..end].copy_from_slice(&prepared.original_data);
            page.dirty_generation = page.dirty_generation.wrapping_add(1);
            page.dirty = prepared.was_dirty || (!self.in_memory && !backing_restored);
        }
    }

    fn rollback_prepared_backing(&self, file: &FileNode, prepared: &PreparedPageWrite) -> bool {
        if self.in_memory {
            return true;
        }
        let original_backing_data = &prepared.original_data[..prepared.zeroed_data.len()];
        match file.write_at(original_backing_data, Self::prepared_offset(prepared)) {
            Ok(written) if written == original_backing_data.len() => true,
            Ok(_) => {
                warn!("short write while rolling back a failed cached-file resize");
                false
            }
            Err(err) => {
                warn!(
                    "failed to restore backing data after cached-file resize error: {:?}",
                    err
                );
                false
            }
        }
    }

    fn restore_backing_length_after_error(
        &self,
        file: &FileNode,
        old_len: u64,
        fallback_len: u64,
    ) -> bool {
        match file.set_len(old_len) {
            Ok(()) => true,
            Err(err) => {
                warn!(
                    "failed to restore backing length after cached-file resize error: {:?}",
                    err
                );
                self.shared.set_len(file.len().unwrap_or(fallback_len));
                false
            }
        }
    }

    fn take_discarded_pages_locked(&self, len: u64) -> Vec<(u32, PageCache)> {
        let first_discarded_page = len.div_ceil(PAGE_SIZE as u64);
        let mut guard = self.shared.page_cache.lock();
        let keys = guard
            .iter()
            .map(|(page_number, _)| *page_number)
            .filter(|page_number| u64::from(*page_number) >= first_discarded_page)
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|page_number| {
                guard.pop(&page_number).map(|mut page| {
                    // Pages wholly beyond the new EOF must never be written
                    // back after the truncate.
                    page.dirty = false;
                    (page_number, page)
                })
            })
            .collect()
    }

    fn notify_discarded_pages(
        &self,
        file: &FileNode,
        pages: Vec<(u32, PageCache)>,
    ) -> VfsResult<()> {
        for (page_number, mut page) in pages {
            self.evict_cache(file, page_number, &mut page)?;
        }
        Ok(())
    }

    /// Truncates or extends the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        let file = self.inner.entry().as_file()?;
        loop {
            let observed_len = self.shared.len();
            let affected_page =
                if observed_len < len && !observed_len.is_multiple_of(PAGE_SIZE as u64) {
                    Some((observed_len / PAGE_SIZE as u64) as u32)
                } else if len < observed_len && !len.is_multiple_of(PAGE_SIZE as u64) {
                    Some((len / PAGE_SIZE as u64) as u32)
                } else {
                    None
                };

            // Revoke writable mappings before snapshotting a partial page.
            // Callbacks may take address-space locks, so invoke them without
            // holding the cached-I/O or page-cache locks.
            if let Some(page_number) = affected_page {
                self.shared
                    .protect_dirty_pages_before_writeback(&[page_number])?;
            }

            let io = self.shared.io_lock.lock();
            let old_len = self.shared.len();
            if old_len != observed_len {
                continue;
            }

            if old_len < len {
                let prepared = if let Some(page_number) = affected_page {
                    let page_start = u64::from(page_number) * PAGE_SIZE as u64;
                    let old_page_offset = (old_len - page_start) as usize;
                    let new_page_offset = (len - page_start).min(PAGE_SIZE as u64) as usize;
                    Some(self.prepare_zero_write_locked(
                        file,
                        page_number,
                        old_page_offset,
                        PAGE_SIZE,
                        new_page_offset,
                    )?)
                } else {
                    None
                };

                if let Err(err) = file.set_len(len) {
                    let length_restored =
                        self.restore_backing_length_after_error(file, old_len, old_len);
                    if length_restored && let Some(prepared) = prepared.as_ref() {
                        self.restore_prepared_cache(prepared, true);
                    }
                    return Err(err);
                }
                if let Some(prepared) = prepared.as_ref()
                    && let Err(err) = self.persist_prepared_page(file, prepared)
                {
                    if self.restore_backing_length_after_error(file, old_len, len) {
                        self.restore_prepared_cache(prepared, true);
                    }
                    return Err(err);
                }

                self.shared.set_len(len);
                if let Some(prepared) = prepared.as_ref() {
                    self.finish_prepared_page(prepared);
                }
                return Ok(());
            }

            if len < old_len {
                let prepared = if let Some(page_number) = affected_page {
                    let page_start = u64::from(page_number) * PAGE_SIZE as u64;
                    let old_page_len = (old_len - page_start).min(PAGE_SIZE as u64) as usize;
                    Some(self.prepare_zero_write_locked(
                        file,
                        page_number,
                        (len - page_start) as usize,
                        PAGE_SIZE,
                        old_page_len,
                    )?)
                } else {
                    None
                };

                if let Some(prepared) = prepared.as_ref()
                    && let Err(err) = self.persist_prepared_page(file, prepared)
                {
                    let restored = self.rollback_prepared_backing(file, prepared);
                    self.restore_prepared_cache(prepared, restored);
                    return Err(err);
                }
                if let Err(err) = file.set_len(len) {
                    let length_restored =
                        self.restore_backing_length_after_error(file, old_len, old_len);
                    if let Some(prepared) = prepared.as_ref() {
                        let restored = self.rollback_prepared_backing(file, prepared);
                        self.restore_prepared_cache(prepared, length_restored && restored);
                    }
                    return Err(err);
                }

                self.shared.set_len(len);
                if let Some(prepared) = prepared.as_ref() {
                    self.finish_prepared_page(prepared);
                }
                let discarded = self.take_discarded_pages_locked(len);
                drop(io);
                self.notify_discarded_pages(file, discarded)?;
                return Ok(());
            }

            file.set_len(len)?;
            self.shared.set_len(len);
            return Ok(());
        }
    }
}
