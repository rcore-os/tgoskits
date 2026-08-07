use axfs_ng_vfs::{VfsError, VfsResult};

use crate::os::memory::{FsPage, PAGE_SIZE};

pub struct PageCache {
    page: Option<FsPage>,
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
            dirty: false,
            dirty_generation: 0,
            writeback_protecting: false,
            dirty_during_writeback: false,
        })
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
            .as_ref()
            .expect("page cache frame already dropped");
        unsafe { core::slice::from_raw_parts_mut(page.as_mut_ptr(), PAGE_SIZE) }
    }
}

impl Drop for PageCache {
    fn drop(&mut self) {
        if self.dirty {
            warn!("dirty page dropped without flushing");
        }
        if let Some(page) = self.page.take() {
            crate::os::memory::dealloc_page(page);
        }
    }
}
