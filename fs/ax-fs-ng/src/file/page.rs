#[cfg(test)]
use alloc::sync::Arc;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

use axfs_ng_vfs::{VfsError, VfsResult};

use crate::os::memory::FsPage;

pub struct PageCache {
    page: Option<FsPage>,
    #[cfg(test)]
    dirty_drop_observer: Option<Arc<AtomicUsize>>,
    /// Transient users that hold the frame identity outside the cache-index
    /// lock while publishing or validating a PTE.
    pub(super) pins: usize,
    pub(super) dirty: bool,
    pub(super) dirty_generation: u64,
    pub(super) writeback_protecting: bool,
    pub(super) dirty_during_writeback: bool,
}

impl PageCache {
    pub(super) fn new() -> VfsResult<Self> {
        let page = crate::os::alloc_page().map_err(|err| {
            warn!("Failed to allocate page cache: {:?}", err);
            VfsError::NoMemory
        })?;
        Ok(Self {
            page: Some(page),
            #[cfg(test)]
            dirty_drop_observer: None,
            pins: 0,
            dirty: false,
            dirty_generation: 0,
            writeback_protecting: false,
            dirty_during_writeback: false,
        })
    }

    #[cfg(all(test, feature = "vfs"))]
    pub(super) const fn detached_for_test() -> Self {
        Self {
            page: None,
            dirty_drop_observer: None,
            pins: 0,
            dirty: false,
            dirty_generation: 0,
            writeback_protecting: false,
            dirty_during_writeback: false,
        }
    }

    /// Returns the physical address of this page.
    pub fn paddr(&self) -> VfsResult<usize> {
        let page = self.page.as_ref().ok_or(VfsError::BadState)?;
        crate::os::virt_to_phys(page.addr()).ok_or(VfsError::BadState)
    }

    /// Marks this page as dirty so it will be flushed on eviction.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        if self.writeback_protecting {
            self.dirty_during_writeback = true;
        }
        self.dirty_generation = self.dirty_generation.wrapping_add(1);
    }

    /// Returns a mutable slice over the page data.
    pub fn data(&mut self) -> &mut [u8] {
        let page = self
            .page
            .as_mut()
            .expect("page cache frame already dropped");
        page.as_mut_slice()
    }

    /// Retires a page whose cached contents were invalidated by a file-layout
    /// change rather than persisted by writeback.
    ///
    /// The caller must first retire every mapping of this frame. Consuming the
    /// owner makes it impossible to accidentally restore invalidated dirty
    /// contents to the cache after this transition.
    pub(super) fn retire_invalidated(mut self) {
        self.dirty = false;
    }

    #[cfg(test)]
    pub(super) fn observe_dirty_drop(&mut self, observer: Arc<AtomicUsize>) {
        self.dirty_drop_observer = Some(observer);
    }
}

impl Drop for PageCache {
    fn drop(&mut self) {
        if self.dirty {
            #[cfg(test)]
            if let Some(observer) = &self.dirty_drop_observer {
                observer.fetch_add(1, Ordering::AcqRel);
            }
            warn!("dirty page dropped without flushing");
        }
        if let Some(page) = self.page.take() {
            crate::os::memory::dealloc_page(page);
        }
    }
}
