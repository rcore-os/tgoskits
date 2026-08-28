use alloc::vec::Vec;

use axfs_ng_vfs::{FileNode, FileRangeOperation, PreallocationMode, VfsError, VfsResult};

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
    /// Reserves backing storage and keeps the cached length coherent.
    pub fn preallocate(&self, offset: u64, len: u64, mode: PreallocationMode) -> VfsResult<()> {
        let end = offset.checked_add(len).ok_or(VfsError::FileTooLarge)?;
        let file = self.inner.entry().as_file()?;
        let _io = self.shared.io_lock.lock();
        file.preallocate(offset, len, mode)?;
        if mode == PreallocationMode::ExtendSize {
            self.shared.update_len_max(end);
        }
        Ok(())
    }

    /// Applies a mapping-changing range operation without allowing cached
    /// dirty pages to restore the old backing mapping during later writeback.
    pub fn operate_range(
        &self,
        offset: u64,
        len: u64,
        operation: FileRangeOperation,
    ) -> VfsResult<()> {
        if let FileRangeOperation::Allocate(mode) = operation {
            return self.preallocate(offset, len, mode);
        }
        if matches!(
            operation,
            FileRangeOperation::CollapseRange | FileRangeOperation::InsertRange
        ) {
            return self.operate_shifted_range(offset, len, operation);
        }
        let end = offset.checked_add(len).ok_or(VfsError::FileTooLarge)?;
        let file = self.inner.entry().as_file()?;

        loop {
            let observed_len = self.shared.len();
            let visible_end = match operation {
                FileRangeOperation::PunchHole
                | FileRangeOperation::ZeroRange(PreallocationMode::KeepSize) => {
                    core::cmp::min(end, observed_len)
                }
                FileRangeOperation::ZeroRange(PreallocationMode::ExtendSize) => end,
                FileRangeOperation::Allocate(_) => unreachable!(),
                FileRangeOperation::CollapseRange | FileRangeOperation::InsertRange => {
                    unreachable!()
                }
            };
            let start_page = offset / PAGE_SIZE as u64;
            let end_page = visible_end.div_ceil(PAGE_SIZE as u64);
            let affected_pages = {
                let guard = self.shared.page_cache.lock();
                guard
                    .iter()
                    .filter_map(|(&page_number, _)| {
                        let page = u64::from(page_number);
                        (start_page <= page && page < end_page).then_some(page_number)
                    })
                    .collect::<Vec<_>>()
            };

            self.shared
                .protect_dirty_pages_before_writeback(&affected_pages)?;
            if !self.in_memory && !affected_pages.is_empty() {
                self.writeback_pages(&affected_pages)?;
            }

            let _io = self.shared.io_lock.lock();
            if self.shared.len() != observed_len {
                continue;
            }
            if !self.in_memory {
                let mut guard = self.shared.page_cache.lock();
                if affected_pages
                    .iter()
                    .any(|page_number| guard.get(page_number).is_some_and(|page| page.dirty))
                {
                    continue;
                }
            }

            file.operate_range(offset, len, operation)?;
            let mut guard = self.shared.page_cache.lock();
            for page_number in affected_pages {
                let Some(page) = guard.get_mut(&page_number) else {
                    continue;
                };
                let page_start = u64::from(page_number) * PAGE_SIZE as u64;
                let page_end = page_start + PAGE_SIZE as u64;
                let zero_start = core::cmp::max(offset, page_start) - page_start;
                let zero_end = core::cmp::min(visible_end, page_end) - page_start;
                if zero_start < zero_end {
                    page.data()[zero_start as usize..zero_end as usize].fill(0);
                    page.dirty = false;
                    page.dirty_generation = page.dirty_generation.wrapping_add(1);
                }
            }
            drop(guard);
            if operation == FileRangeOperation::ZeroRange(PreallocationMode::ExtendSize) {
                self.shared.update_len_max(end);
            }
            return Ok(());
        }
    }

    fn operate_shifted_range(
        &self,
        offset: u64,
        len: u64,
        operation: FileRangeOperation,
    ) -> VfsResult<()> {
        let file = self.inner.entry().as_file()?;
        let start_page = offset / PAGE_SIZE as u64;
        loop {
            let observed_len = self.shared.len();
            let new_len = match operation {
                FileRangeOperation::CollapseRange => observed_len
                    .checked_sub(len)
                    .ok_or(VfsError::InvalidInput)?,
                FileRangeOperation::InsertRange => observed_len
                    .checked_add(len)
                    .ok_or(VfsError::FileTooLarge)?,
                _ => unreachable!(),
            };
            let affected_pages = self.cached_pages_from(start_page);

            self.shared
                .protect_dirty_pages_before_writeback(&affected_pages)?;
            if !self.in_memory && !affected_pages.is_empty() {
                self.writeback_pages(&affected_pages)?;
            }

            let io = self.shared.io_lock.lock();
            if self.shared.len() != observed_len {
                continue;
            }
            let final_affected_pages = self.cached_pages_from(start_page);
            if final_affected_pages != affected_pages {
                continue;
            }
            if !self.in_memory {
                let mut guard = self.shared.page_cache.lock();
                if final_affected_pages
                    .iter()
                    .any(|page_number| guard.get(page_number).is_some_and(|page| page.dirty))
                {
                    continue;
                }
            }

            file.operate_range(offset, len, operation)?;
            self.shared.set_len(new_len);
            let discarded = {
                let mut guard = self.shared.page_cache.lock();
                final_affected_pages
                    .into_iter()
                    .filter_map(|page_number| {
                        guard.pop(&page_number).map(|mut page| {
                            page.dirty = false;
                            (page_number, page)
                        })
                    })
                    .collect::<Vec<_>>()
            };
            drop(io);
            self.notify_discarded_pages(file, discarded)?;
            return Ok(());
        }
    }

    fn cached_pages_from(&self, start_page: u64) -> Vec<u32> {
        let guard = self.shared.page_cache.lock();
        let mut pages = guard
            .iter()
            .filter_map(|(&page_number, _)| {
                (u64::from(page_number) >= start_page).then_some(page_number)
            })
            .collect::<Vec<_>>();
        pages.sort_unstable();
        pages
    }

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
