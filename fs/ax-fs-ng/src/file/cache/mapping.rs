//! Physical lifetime and permission to publish a file mapping are distinct.

use core::sync::atomic::Ordering;

use axfs_ng_vfs::{VfsError, VfsResult};

use super::{CachedFile, CachedPageBacking, CachedPagePin, PAGE_SIZE};

impl CachedFile {
    /// Loads and transiently pins an initialized, complete file page.
    ///
    /// Partial EOF pages return None and use the consumer's bounded copy path.
    /// A concurrent cache/layout update returns ResourceBusy for MM to retry.
    /// Publication must revalidate the backing with with_current_read_backing.
    pub fn pin_read_page(&self, number: u32) -> VfsResult<Option<CachedPagePin>> {
        let start = u64::from(number) * PAGE_SIZE as u64;
        let end = start + PAGE_SIZE as u64;
        let file = self.inner.entry().as_file()?;
        let window = self.readahead.lock().plan(start, end).window_pages;
        loop {
            {
                let mut cache = self.shared.page_cache.lock();
                if self.shared.updating.load(Ordering::Acquire)
                    || self
                        .shared
                        .mapping_update_in_progress
                        .load(Ordering::Acquire)
                {
                    return Err(VfsError::ResourceBusy);
                }
                if end > self.shared.len() {
                    return Ok(None);
                }
                if let Some(page) = cache.get_mut(&number) {
                    let paddr = page.paddr()?;
                    page.pins = page.pins.checked_add(1).ok_or(VfsError::ValueOverflow)?;
                    return Ok(Some(CachedPagePin {
                        shared: self.shared.clone(),
                        page_number: number,
                        paddr,
                        backing: page.backing(),
                    }));
                }
            }
            self.populate_page_window(file, number, window)?;
        }
    }

    /// Applies a mapping mutation only while backing and epoch are current.
    ///
    /// The caller already owns MM metadata. `publish` runs under cache-index
    /// exclusion and must not allocate, enter cached I/O, invoke callbacks,
    /// acquire MM metadata, or access faultable memory. The operation grants
    /// no writable alias; the mapping retains physical backing through TLB
    /// retirement. None leaves the callback unexecuted and requests retry.
    pub fn with_current_read_backing<T, E>(
        &self,
        number: u32,
        epoch: u64,
        backing: &CachedPageBacking,
        publish: impl FnOnce() -> Result<T, E>,
    ) -> Result<Option<T>, E> {
        let mut cache = self.shared.page_cache.lock();
        let end = (u64::from(number) + 1) * PAGE_SIZE as u64;
        if self.shared.updating.load(Ordering::Acquire)
            || self
                .shared
                .mapping_update_in_progress
                .load(Ordering::Acquire)
            || self.mapping_epoch() != epoch
            || end > self.shared.len()
            || !cache
                .get_mut(&number)
                .is_some_and(|page| page.matches_backing(backing))
        {
            return Ok(None);
        }
        publish().map(Some)
    }
}
