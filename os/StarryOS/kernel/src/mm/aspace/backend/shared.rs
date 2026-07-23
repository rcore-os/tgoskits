use alloc::sync::Arc;

use ax_errno::AxResult;
use ax_memory_addr::{MemoryAddr, VirtAddr, VirtAddrRange};
use ax_runtime::hal::paging::{MappingFlags, PageSize, PageTableCursor, PagingError};
use ax_sync::Mutex;
use starry_mm::SharedPages;

use super::{
    AddrSpace, Backend, BackendOps, CloneMapAccounting, MemoryAccounting, RssKind, divide_page,
    pages_in,
};

#[derive(Clone)]
pub struct SharedBackend {
    start: VirtAddr,
    pages: Arc<SharedPages>,
    page_offset: usize,
}
impl SharedBackend {
    pub fn pages(&self) -> &Arc<SharedPages> {
        &self.pages
    }

    /// Returns a clone with a different start address.
    pub fn with_start(&self, new_start: VirtAddr) -> Self {
        Self {
            start: new_start,
            pages: self.pages.clone(),
            page_offset: self.page_offset,
        }
    }
}

impl BackendOps for SharedBackend {
    fn page_size(&self) -> PageSize {
        self.pages.page_size()
    }

    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        debug!("Shared::map: {:?} {:?}", range, flags);
        debug_assert!(range.start.is_aligned(self.pages.page_size()));
        let start_index =
            self.page_offset + divide_page(range.start - self.start, self.pages.page_size());
        for (vaddr, paddr) in
            pages_in(range, self.pages.page_size())?.zip(&self.pages[start_index..])
        {
            let newly_mapped = pt.query(vaddr).is_err();
            pt.map(vaddr, *paddr, self.pages.page_size(), flags)?;
            if newly_mapped && let Some(acct) = acct {
                acct.inc(RssKind::Shmem, 1);
            }
        }
        Ok(())
    }

    fn unmap(
        &self,
        range: VirtAddrRange,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        debug!("Shared::unmap: {:?}", range);
        for vaddr in pages_in(range, self.pages.page_size())? {
            match pt.unmap(vaddr) {
                Ok((_, _, page_size)) => {
                    debug_assert_eq!(page_size, self.pages.page_size());
                    if let Some(acct) = acct {
                        acct.dec(RssKind::Shmem, 1);
                    }
                }
                Err(PagingError::NotMapped) => {}
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }

    fn clone_map(
        &self,
        _range: VirtAddrRange,
        _flags: MappingFlags,
        _old_pt: &mut PageTableCursor,
        _new_pt: &mut PageTableCursor,
        _new_aspace: &Arc<Mutex<AddrSpace>>,
        _acct: CloneMapAccounting<'_>,
    ) -> AxResult<Backend> {
        Ok(Backend::Shared(self.clone()))
    }

    fn split(&mut self, align_diff: usize) -> Option<Backend> {
        if align_diff == 0 {
            return None;
        }
        Some(Backend::Shared(SharedBackend {
            start: self.start + align_diff,
            pages: self.pages.clone(),
            page_offset: self.page_offset + divide_page(align_diff, self.pages.page_size()),
        }))
    }
}

impl Backend {
    pub fn new_shared(start: VirtAddr, pages: Arc<SharedPages>) -> Self {
        Self::Shared(SharedBackend {
            start,
            pages,
            page_offset: 0,
        })
    }
}
