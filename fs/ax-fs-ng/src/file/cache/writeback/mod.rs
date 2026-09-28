//! File-scoped writeback rounds with lock-external owned I/O batches.

#[cfg(all(feature = "ext4", feature = "vfs"))]
use alloc::sync::Arc;
use alloc::vec::Vec;

use axfs_ng_vfs::{VfsError, VfsResult};

#[cfg(feature = "vfs")]
use super::DIRTY_PAGE_HARD_WATERMARK;
use super::{
    CacheMappingEvent, CacheMappingResult, CachedFileShared, DIRTY_PAGE_BACKGROUND_WATERMARK,
    DIRTY_PAGE_LOW_WATERMARK, PAGE_SIZE,
};

mod batch;
mod pages;

struct WritebackPage {
    number: u32,
    paddr: usize,
}

struct WritebackPages<'a> {
    shared: &'a CachedFileShared,
    file_len: u64,
    pages: Vec<WritebackPage>,
}

impl CachedFileShared {
    /// Runs after the writer publishes its current stable page and releases
    /// io_lock. Concurrent writeback may temporarily exceed the soft target.
    pub(super) fn balance_dirty_pages(&self, growing: bool) -> VfsResult<()> {
        let over_capacity = {
            let cache = self.page_cache.lock();
            cache.len() > cache.cap().get()
        };
        // Growth already extends the bounded retention target before page
        // publication. Keep those bytes cached instead of applying the fixed
        // overwrite watermarks; explicit and periodic sync still flush them.
        if growing && !over_capacity {
            return Ok(());
        }
        let dirty_count = self.dirty_page_count();
        let flush_to_low_watermark = dirty_count >= DIRTY_PAGE_BACKGROUND_WATERMARK;
        #[cfg(feature = "vfs")]
        let flush_to_low_watermark = if flush_to_low_watermark {
            self.request_background_writeback();
            if super::writeback_worker::request_background_writeback() {
                dirty_count >= DIRTY_PAGE_HARD_WATERMARK
            } else {
                self.take_background_writeback_request();
                true
            }
        } else {
            false
        };
        if self.has_mapping_endpoint() || (!over_capacity && !flush_to_low_watermark) {
            return Ok(());
        }
        // Buffered redirty can occur inside an explicit round's backing call.
        // It must never recursively wait for that same writeback owner.
        let Some(_writeback) = self.writeback_lock.try_lock() else {
            return Ok(());
        };
        let count = if flush_to_low_watermark {
            dirty_count.saturating_sub(DIRTY_PAGE_LOW_WATERMARK)
        } else {
            1
        };
        let selected = self.oldest_dirty_pages(count)?;
        let round = WritebackPages::begin(self, Some(&selected))?;
        round.protect()?;
        round.write_back()?;
        drop(round);
        if over_capacity {
            loop {
                let retired = {
                    let mut cache = self.page_cache.lock();
                    if cache.len() <= cache.cap().get() {
                        break;
                    }
                    self.detach_clean_capacity_victim(&mut cache)
                };
                if retired.is_none() {
                    break;
                }
                drop(retired);
            }
        }
        Ok(())
    }

    pub(super) fn dirty_page_count(&self) -> usize {
        self.page_cache
            .lock()
            .iter()
            .filter(|(_, page)| page.dirty)
            .count()
    }

    fn oldest_dirty_pages(&self, count: usize) -> VfsResult<Vec<u32>> {
        let _io = self.io_lock.lock();
        let mut selected = Vec::new();
        selected
            .try_reserve_exact(count)
            .map_err(|_| VfsError::NoMemory)?;
        selected.extend(
            self.page_cache
                .lock()
                .iter()
                .rev()
                .filter_map(|(&number, page)| page.dirty.then_some(number))
                .take(count),
        );
        Ok(selected)
    }

    #[cfg(feature = "vfs")]
    pub(super) fn writeback_dirty_for_background(&self) -> VfsResult<()> {
        let _writeback = self.writeback_lock.lock();
        let count = self.dirty_page_count();
        if count < DIRTY_PAGE_BACKGROUND_WATERMARK {
            return Ok(());
        }
        let selected = self.oldest_dirty_pages(count - DIRTY_PAGE_LOW_WATERMARK)?;
        self.writeback_registered_pages(Some(&selected))
    }

    #[cfg(feature = "vfs")]
    pub(super) fn writeback_dirty_for_periodic(&self) -> VfsResult<()> {
        let _writeback = self.writeback_lock.lock();
        self.writeback_registered_pages(None)
    }

    #[cfg(any(feature = "vfs", feature = "ext4"))]
    pub(super) fn writeback_dirty_for_global_sync(&self) -> VfsResult<()> {
        let _writeback = self.writeback_lock.lock();
        self.writeback_registered_pages(None)
    }

    #[cfg(any(feature = "vfs", feature = "ext4"))]
    fn writeback_registered_pages(&self, requested: Option<&[u32]>) -> VfsResult<()> {
        let round = WritebackPages::begin(self, requested)?;
        round.protect()?;
        #[cfg(feature = "vfs")]
        if self.retired.load(core::sync::atomic::Ordering::Acquire)
            || self.unlinked.load(core::sync::atomic::Ordering::Acquire)
        {
            return Ok(());
        }
        round.write_back()
    }

    pub(super) fn writeback(&self) -> VfsResult<Vec<u32>> {
        let _writeback = self.writeback_lock.lock();
        let round = WritebackPages::begin(self, None)?;
        let numbers = round.numbers()?;
        round.protect()?;
        round.write_back()?;
        drop(round);
        self.backing()?.sync(false)?;
        Ok(numbers)
    }

    pub(super) fn writeback_pages(&self, pns: &[u32]) -> VfsResult<()> {
        let _writeback = self.writeback_lock.lock();
        let round = WritebackPages::begin(self, Some(pns))?;
        round.protect()?;
        round.write_back()?;
        drop(round);
        self.backing()?.sync(false)
    }

    pub(super) fn sync(&self, data_only: bool) -> VfsResult<()> {
        let _writeback = self.writeback_lock.lock();
        let round = WritebackPages::begin(self, None)?;
        round.protect()?;
        round.write_back()?;
        drop(round);
        self.backing()?.sync(data_only)
    }

    #[cfg(all(feature = "ext4", feature = "vfs"))]
    pub(super) fn retire_from_writeback_registry(self: &Arc<Self>) -> VfsResult<()> {
        #[cfg(test)]
        self.notify_retirement_lock_attempt();
        let _writeback = self.writeback_lock.lock();
        {
            let _io = self.io_lock.lock();
            self.retired
                .store(true, core::sync::atomic::Ordering::Release);
        }
        let result = (|| {
            let round = WritebackPages::begin(self, None)?;
            round.protect()?;
            round.write_back()
        })();
        if let Err(error) = result {
            let _io = self.io_lock.lock();
            self.retired
                .store(false, core::sync::atomic::Ordering::Release);
            return Err(error);
        }
        super::reclaim::release_cached_file(self);
        Ok(())
    }

    #[cfg(feature = "vfs")]
    pub(super) fn has_dirty_pages(&self) -> bool {
        self.page_cache.lock().iter().any(|(_, page)| page.dirty)
    }

    pub(super) fn protect_dirty_pages_before_writeback(&self, pns: &[u32]) -> VfsResult<()> {
        for pn in pns {
            let paddr = {
                let mut cache = self.page_cache.lock();
                cache.get_mut(pn).map(|page| page.paddr()).transpose()?
            };
            let Some(paddr) = paddr else {
                continue;
            };
            match self.publish_mapping_event(CacheMappingEvent::WritebackProtect(
                self.cache_page_identity(*pn, paddr),
            )) {
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
}
