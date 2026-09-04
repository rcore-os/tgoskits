use alloc::vec::Vec;

use axfs_ng_vfs::{FileNode, FileRangeOperation, PreallocationMode, VfsError, VfsResult};

use super::{CacheMappingEvent, CacheMappingResult, CachedFile, PAGE_SIZE, PageCache};

struct PreparedPageWrite {
    page_number: u32,
    page_offset: usize,
    generation: u64,
    was_dirty: bool,
    original_data: Vec<u8>,
    zeroed_data: Vec<u8>,
}

/// Move-only ownership of cache pages detached for one mapping update.
///
/// Every vector is reserved before a page leaves the cache index. Callback
/// failure can therefore retain every exact frame owner until it is restored.
struct DetachedPageBatch {
    pages: Vec<(u32, PageCache)>,
    retired: Vec<(u32, PageCache)>,
    replaced: Vec<PageCache>,
}

/// Detached frame owners whose reverse mappings have been retired.
///
/// Retirement is not final invalidation: the pages remain owned here until the
/// backing mutation commits. An error can therefore restore the same dirty
/// cache objects instead of reconstructing data from stale backing storage.
#[must_use = "retired cache owners must be restored or finalized after backing commit"]
struct RetiredPageBatch {
    pages: Vec<(u32, PageCache)>,
    replaced: Vec<PageCache>,
}

impl DetachedPageBatch {
    fn prepare(capacity: usize) -> VfsResult<Self> {
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(capacity)
            .map_err(|_| VfsError::NoMemory)?;
        let mut retired = Vec::new();
        retired
            .try_reserve_exact(capacity)
            .map_err(|_| VfsError::NoMemory)?;
        let mut replaced = Vec::new();
        replaced
            .try_reserve_exact(capacity)
            .map_err(|_| VfsError::NoMemory)?;
        Ok(Self {
            pages,
            retired,
            replaced,
        })
    }

    fn restore(mut self, cached: &CachedFile) {
        let mut guard = cached.shared.page_cache.lock();
        while let Some((page_number, page)) = self.pages.pop() {
            if let Some(page) = guard.put(page_number, page) {
                self.replaced.push(page);
            }
        }
        while let Some((page_number, page)) = self.retired.pop() {
            if let Some(page) = guard.put(page_number, page) {
                self.replaced.push(page);
            }
        }
        drop(guard);
        // A replaced cache owner may release its frame. Keep all final owner
        // drops outside the cache-index lock.
        drop(self.replaced);
    }

    fn into_retired(self) -> RetiredPageBatch {
        let Self {
            pages,
            retired,
            replaced,
        } = self;
        debug_assert!(pages.is_empty());
        drop(pages);
        RetiredPageBatch {
            pages: retired,
            replaced,
        }
    }
}

impl RetiredPageBatch {
    fn restore(mut self, cached: &CachedFile) {
        let mut guard = cached.shared.page_cache.lock();
        while let Some((page_number, page)) = self.pages.pop() {
            if let Some(page) = guard.put(page_number, page) {
                self.replaced.push(page);
            }
        }
        drop(guard);
        drop(self.replaced);
    }

    fn retire_invalidated(self) {
        let Self { pages, replaced } = self;
        for (_, page) in pages {
            page.retire_invalidated();
        }
        drop(replaced);
    }
}

impl CachedFile {
    /// Reserves backing storage and keeps the cached length coherent.
    pub fn preallocate(&self, offset: u64, len: u64, mode: PreallocationMode) -> VfsResult<()> {
        let end = offset.checked_add(len).ok_or(VfsError::FileTooLarge)?;
        let file = self.inner.entry().as_file()?;
        let _layout = self.shared.mapping_layout_lock.lock();
        let _io = self.shared.io_lock.lock();
        let next_epoch = (mode == PreallocationMode::ExtendSize && end > self.shared.len())
            .then(|| self.shared.prepare_mapping_epoch())
            .transpose()?;
        file.preallocate(offset, len, mode)?;
        if let Some(next_epoch) = next_epoch {
            self.shared.update_len_max(end);
            self.shared.publish_mapping_epoch(next_epoch);
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
            // Fault invalidation callbacks may re-enter the page cache through
            // an address space, so this must stay separate from the I/O lock.
            // Keep the publication barrier across the initial cache snapshot,
            // backing mutation, cache update, and epoch publication.
            let _mapping_update = self.begin_mapping_update()?;
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
            let affected_pages = self.cached_pages_in(start_page, end_page)?;

            self.shared
                .protect_dirty_pages_before_writeback(&affected_pages)?;
            if !self.in_memory && !affected_pages.is_empty() {
                self.writeback_pages(&affected_pages)?;
            }

            let _io = self.shared.io_lock.lock();
            if self.shared.len() != observed_len
                || !self.cached_page_set_matches(start_page, end_page, &affected_pages)
            {
                // The mapping-layout guard excludes buffered publication, and
                // the fault barrier excludes mmap publication. Keep this
                // second snapshot as an invariant check before backing change.
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

            let next_epoch = self.shared.prepare_mapping_epoch()?;
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
            self.shared.publish_mapping_epoch(next_epoch);
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
        let range_end = offset.checked_add(len).ok_or(VfsError::FileTooLarge)?;
        if len == 0 {
            return Err(VfsError::InvalidInput);
        }
        loop {
            let _mapping_update = self.begin_mapping_update()?;
            let observed_len = self.shared.len();
            let new_len = match operation {
                FileRangeOperation::CollapseRange => {
                    // Linux rejects a collapse that reaches or crosses EOF;
                    // that case is truncate, not a shifted-range operation.
                    if range_end >= observed_len {
                        return Err(VfsError::InvalidInput);
                    }
                    observed_len
                        .checked_sub(len)
                        .ok_or(VfsError::InvalidInput)?
                }
                FileRangeOperation::InsertRange => {
                    if offset >= observed_len {
                        return Err(VfsError::InvalidInput);
                    }
                    observed_len
                        .checked_add(len)
                        .ok_or(VfsError::FileTooLarge)?
                }
                _ => unreachable!(),
            };
            // Reserve the infallible publication value before writeback
            // protection or reverse-mapping retirement has side effects.
            let next_epoch = self.shared.prepare_mapping_epoch()?;
            let affected_pages = self.cached_pages_from(start_page)?;

            self.shared
                .protect_dirty_pages_before_writeback(&affected_pages)?;
            if !self.in_memory && !affected_pages.is_empty() {
                self.writeback_pages(&affected_pages)?;
            }

            let mut discarded = DetachedPageBatch::prepare(affected_pages.len())?;
            let io = self.shared.io_lock.lock();
            if self.shared.len() != observed_len {
                continue;
            }
            if !self.cached_page_set_matches(start_page, u64::MAX, &affected_pages) {
                continue;
            }
            let cache_busy = {
                let mut guard = self.shared.page_cache.lock();
                affected_pages.iter().any(|page_number| {
                    guard
                        .get(page_number)
                        .is_some_and(|page| page.pins != 0 || (!self.in_memory && page.dirty))
                })
            };
            if cache_busy {
                return Err(VfsError::ResourceBusy);
            }
            {
                let mut guard = self.shared.page_cache.lock();
                for page_number in affected_pages {
                    if let Some(page) = guard.pop(&page_number) {
                        discarded.pages.push((page_number, page));
                    }
                }
            }
            drop(io);
            let retired = self.notify_discarded_pages(discarded)?;

            // Invalidators can take address-space locks, so the I/O lock was
            // deliberately released. Recheck both the file generation and
            // cache index before making the backing shift irreversible.
            let io = self.shared.io_lock.lock();
            if self.shared.len() != observed_len
                || !self.cached_page_set_matches(start_page, u64::MAX, &[])
            {
                drop(io);
                retired.restore(self);
                continue;
            }
            if let Err(error) = file.operate_range(offset, len, operation) {
                drop(io);
                retired.restore(self);
                return Err(error);
            }
            self.shared.set_len(new_len);
            self.shared.publish_mapping_epoch(next_epoch);
            drop(io);
            retired.retire_invalidated();
            return Ok(());
        }
    }

    fn cached_pages_from(&self, start_page: u64) -> VfsResult<Vec<u32>> {
        self.cached_pages_in(start_page, u64::MAX)
    }

    /// Takes a sorted cache-index snapshot without growing a vector while the
    /// cache lock is held. The first pass only measures, allocation happens
    /// lock-free, and the second pass retries if concurrent insertion exceeded
    /// the prepared capacity.
    pub(super) fn cached_pages_in(&self, start_page: u64, end_page: u64) -> VfsResult<Vec<u32>> {
        let contains = |page_number: u32| {
            let page = u64::from(page_number);
            start_page <= page && page < end_page
        };
        let mut pages = Vec::new();
        loop {
            pages.clear();
            let required = {
                let guard = self.shared.page_cache.lock();
                guard
                    .iter()
                    .filter(|(page_number, _)| contains(**page_number))
                    .count()
            };
            if pages.capacity() < required {
                pages
                    .try_reserve_exact(required)
                    .map_err(|_| VfsError::NoMemory)?;
            }

            let guard = self.shared.page_cache.lock();
            let mut overflowed = false;
            for (&page_number, _) in guard.iter() {
                if !contains(page_number) {
                    continue;
                }
                if pages.len() == pages.capacity() {
                    overflowed = true;
                    break;
                }
                pages.push(page_number);
            }
            drop(guard);
            if overflowed {
                continue;
            }
            pages.sort_unstable();
            return Ok(pages);
        }
    }

    /// Rechecks a sorted snapshot without allocating. Callers hold both the
    /// mapping-layout guard and `io_lock`; mmap faults are excluded by the
    /// mapping-update publication barrier.
    fn cached_page_set_matches(&self, start_page: u64, end_page: u64, expected: &[u32]) -> bool {
        let guard = self.shared.page_cache.lock();
        let mut actual_len = 0;
        for (&page_number, _) in guard.iter() {
            let page = u64::from(page_number);
            if page < start_page || page >= end_page {
                continue;
            }
            actual_len += 1;
            if expected.binary_search(&page_number).is_err() {
                return false;
            }
        }
        actual_len == expected.len()
    }

    pub(super) fn zero_partial_page_locked(
        &self,
        file: &FileNode,
        page_number: u32,
        zero_start: usize,
        zero_end: usize,
    ) -> VfsResult<()> {
        self.with_page_or_insert(file, page_number, true, |page, _| {
            page.data()[zero_start..zero_end].fill(0);
            if !self.in_memory {
                page.mark_dirty();
            }
        })
    }

    fn prepare_zero_write_locked(
        &self,
        file: &FileNode,
        page_number: u32,
        zero_start: usize,
        zero_end: usize,
        persist_end: usize,
    ) -> VfsResult<PreparedPageWrite> {
        let original_len = zero_end
            .checked_sub(zero_start)
            .filter(|_| zero_end <= PAGE_SIZE)
            .ok_or(VfsError::InvalidInput)?;
        let zeroed_len = persist_end
            .checked_sub(zero_start)
            .filter(|_| persist_end <= PAGE_SIZE)
            .ok_or(VfsError::InvalidInput)?;
        let mut original_data = Vec::new();
        original_data
            .try_reserve_exact(original_len)
            .map_err(|_| VfsError::NoMemory)?;
        let mut zeroed_data = Vec::new();
        zeroed_data
            .try_reserve_exact(zeroed_len)
            .map_err(|_| VfsError::NoMemory)?;
        let (was_dirty, generation) =
            self.with_page_or_insert(file, page_number, true, |page, _| {
                let was_dirty = page.dirty;
                original_data.extend_from_slice(&page.data()[zero_start..zero_end]);
                page.data()[zero_start..zero_end].fill(0);
                if !self.in_memory {
                    page.mark_dirty();
                }
                let generation = page.dirty_generation;
                zeroed_data.extend_from_slice(&page.data()[zero_start..persist_end]);
                (was_dirty, generation)
            })?;
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

    fn take_discarded_pages_locked(&self, len: u64) -> VfsResult<DetachedPageBatch> {
        let first_discarded_page = len.div_ceil(PAGE_SIZE as u64);
        let keys = self.cached_pages_from(first_discarded_page)?;
        let mut discarded = DetachedPageBatch::prepare(keys.len())?;
        let mut guard = self.shared.page_cache.lock();
        if keys
            .iter()
            .any(|page_number| guard.get(page_number).is_some_and(|page| page.pins != 0))
        {
            return Err(VfsError::ResourceBusy);
        }
        for page_number in keys {
            if let Some(page) = guard.pop(&page_number) {
                discarded.pages.push((page_number, page));
            }
        }
        Ok(discarded)
    }

    fn notify_discarded_pages(&self, mut batch: DetachedPageBatch) -> VfsResult<RetiredPageBatch> {
        while let Some((page_number, page)) = batch.pages.pop() {
            let result = page
                .paddr()
                .map(|paddr| {
                    self.shared.publish_mapping_event(CacheMappingEvent::Evict(
                        self.cache_page_identity(page_number, paddr),
                    ))
                })
                .unwrap_or(CacheMappingResult::Failed);
            let error = match result {
                CacheMappingResult::Retired => {
                    batch.retired.push((page_number, page));
                    continue;
                }
                CacheMappingResult::Busy | CacheMappingResult::Quarantined => {
                    VfsError::ResourceBusy
                }
                CacheMappingResult::Protected | CacheMappingResult::Failed => VfsError::BadState,
            };
            batch.pages.push((page_number, page));
            batch.restore(self);
            return Err(error);
        }
        Ok(batch.into_retired())
    }

    /// Truncates or extends the file to `len` bytes.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        let file = self.inner.entry().as_file()?;
        loop {
            let observed_len = self.shared.len();
            // Reserve the infallible publication value before callbacks,
            // cache removal, zeroing, or backing-file mutation.  In
            // particular an exhausted epoch must not evict pages and only
            // then report ValueOverflow.
            let prepared_epoch = (observed_len != len)
                .then(|| self.shared.prepare_mapping_epoch())
                .transpose()?;
            let _mapping_update = (observed_len != len)
                .then(|| self.begin_mapping_update())
                .transpose()?;
            if self.shared.len() != observed_len
                || prepared_epoch
                    .is_some_and(|epoch| !self.shared.prepared_mapping_epoch_is_current(epoch))
            {
                continue;
            }
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

            let mut retired_pages = None;
            if len < observed_len {
                let io = self.shared.io_lock.lock();
                if self.shared.len() != observed_len
                    || prepared_epoch
                        .is_some_and(|epoch| !self.shared.prepared_mapping_epoch_is_current(epoch))
                {
                    continue;
                }
                let discarded = self.take_discarded_pages_locked(len)?;
                drop(io);
                // Revoke every mapping and complete its TLB obligation before
                // the backing truncate can make those pages invalid. Keep the
                // exact frame owners until the backing operation commits.
                let retired = self.notify_discarded_pages(discarded)?;
                if let Some(page_number) = affected_page {
                    match self.invalidate_page_mappings(page_number) {
                        Ok(true) => {}
                        Ok(false) => {
                            retired.restore(self);
                            return Err(VfsError::ResourceBusy);
                        }
                        Err(error) => {
                            retired.restore(self);
                            return Err(error);
                        }
                    }
                }
                retired_pages = Some(retired);
            }

            let io = self.shared.io_lock.lock();
            let old_len = self.shared.len();
            if old_len != observed_len
                || prepared_epoch
                    .is_some_and(|epoch| !self.shared.prepared_mapping_epoch_is_current(epoch))
            {
                drop(io);
                if let Some(retired) = retired_pages.take() {
                    retired.restore(self);
                }
                continue;
            }
            if retired_pages.is_some() != (len < old_len) {
                drop(io);
                if let Some(retired) = retired_pages.take() {
                    retired.restore(self);
                }
                return Err(VfsError::BadState);
            }

            if old_len < len {
                let Some(next_epoch) = prepared_epoch else {
                    drop(io);
                    if let Some(retired) = retired_pages.take() {
                        retired.restore(self);
                    }
                    return Err(VfsError::BadState);
                };
                let prepared = if let Some(page_number) = affected_page {
                    let page_start = u64::from(page_number) * PAGE_SIZE as u64;
                    let old_page_offset = (old_len - page_start) as usize;
                    let new_page_offset = (len - page_start).min(PAGE_SIZE as u64) as usize;
                    match self.prepare_zero_write_locked(
                        file,
                        page_number,
                        old_page_offset,
                        PAGE_SIZE,
                        new_page_offset,
                    ) {
                        Ok(prepared) => Some(prepared),
                        Err(error) => {
                            drop(io);
                            return Err(error);
                        }
                    }
                } else {
                    None
                };

                if let Err(err) = file.set_len(len) {
                    let length_restored =
                        self.restore_backing_length_after_error(file, old_len, old_len);
                    if length_restored && let Some(prepared) = prepared.as_ref() {
                        self.restore_prepared_cache(prepared, true);
                    }
                    drop(io);
                    return Err(err);
                }
                if let Some(prepared) = prepared.as_ref()
                    && let Err(err) = self.persist_prepared_page(file, prepared)
                {
                    if self.restore_backing_length_after_error(file, old_len, len) {
                        self.restore_prepared_cache(prepared, true);
                    }
                    drop(io);
                    return Err(err);
                }

                self.shared.set_len(len);
                self.shared.publish_mapping_epoch(next_epoch);
                if let Some(prepared) = prepared.as_ref() {
                    self.finish_prepared_page(prepared);
                }
                drop(io);
                return Ok(());
            }

            if len < old_len {
                if !self.cached_page_set_matches(len.div_ceil(PAGE_SIZE as u64), u64::MAX, &[]) {
                    // This is unreachable while the layout lock and fault
                    // publication barrier are held, but preserve exact owners
                    // if a future cache-population path violates that contract.
                    drop(io);
                    if let Some(retired) = retired_pages.take() {
                        retired.restore(self);
                    }
                    continue;
                }
                let Some(next_epoch) = prepared_epoch else {
                    drop(io);
                    if let Some(retired) = retired_pages.take() {
                        retired.restore(self);
                    }
                    return Err(VfsError::BadState);
                };
                let prepared = if let Some(page_number) = affected_page {
                    let page_start = u64::from(page_number) * PAGE_SIZE as u64;
                    let old_page_len = (old_len - page_start).min(PAGE_SIZE as u64) as usize;
                    match self.prepare_zero_write_locked(
                        file,
                        page_number,
                        (len - page_start) as usize,
                        PAGE_SIZE,
                        old_page_len,
                    ) {
                        Ok(prepared) => Some(prepared),
                        Err(error) => {
                            drop(io);
                            if let Some(retired) = retired_pages.take() {
                                retired.restore(self);
                            }
                            return Err(error);
                        }
                    }
                } else {
                    None
                };

                if let Some(prepared) = prepared.as_ref()
                    && let Err(err) = self.persist_prepared_page(file, prepared)
                {
                    let restored = self.rollback_prepared_backing(file, prepared);
                    self.restore_prepared_cache(prepared, restored);
                    drop(io);
                    if let Some(retired) = retired_pages.take() {
                        retired.restore(self);
                    }
                    return Err(err);
                }
                if let Err(err) = file.set_len(len) {
                    let length_restored =
                        self.restore_backing_length_after_error(file, old_len, old_len);
                    if let Some(prepared) = prepared.as_ref() {
                        let restored = self.rollback_prepared_backing(file, prepared);
                        self.restore_prepared_cache(prepared, length_restored && restored);
                    }
                    drop(io);
                    if let Some(retired) = retired_pages.take() {
                        retired.restore(self);
                    }
                    return Err(err);
                }

                self.shared.set_len(len);
                self.shared.publish_mapping_epoch(next_epoch);
                if let Some(prepared) = prepared.as_ref() {
                    self.finish_prepared_page(prepared);
                }
                drop(io);
                if let Some(retired) = retired_pages {
                    retired.retire_invalidated();
                }
                return Ok(());
            }

            file.set_len(len)?;
            self.shared.set_len(len);
            drop(io);
            return Ok(());
        }
    }
}
