use alloc::{sync::Arc, vec::Vec};
use core::{any::Any, ops::Deref};

use ax_errno::AxResult;
use ax_memory_addr::{MemoryAddr, PhysAddr, VirtAddr, VirtAddrRange};
use ax_runtime::hal::paging::{MappingFlags, PageSize, PageTableCursor, PagingError};
use ax_sync::Mutex;

use super::{
    AddrSpace, Backend, BackendOps, CloneMapAccounting, MemoryAccounting, RssKind, alloc_frame,
    dealloc_frame, divide_page, pages_in,
};

enum SharedPagesOwner {
    Allocated,
    Borrowed(Option<Arc<dyn Any + Send + Sync>>),
}

pub struct SharedPages {
    phys_pages: Vec<PhysAddr>,
    pub size: PageSize,
    owner: SharedPagesOwner,
}
impl SharedPages {
    pub fn new(size: usize, page_size: PageSize) -> AxResult<Self> {
        let num_pages = divide_page(size, page_size);
        let mut result = Self {
            phys_pages: Vec::with_capacity(num_pages),
            size: page_size,
            owner: SharedPagesOwner::Allocated,
        };
        for _ in 0..num_pages {
            result.phys_pages.push(alloc_frame(true, page_size)?);
        }
        Ok(result)
    }

    pub fn borrowed(
        phys_pages: Vec<PhysAddr>,
        page_size: PageSize,
        retain: Option<Arc<dyn Any + Send + Sync>>,
    ) -> AxResult<Self> {
        if phys_pages.is_empty() {
            return Err(ax_errno::AxError::InvalidInput);
        }
        Ok(Self {
            phys_pages,
            size: page_size,
            owner: SharedPagesOwner::Borrowed(retain),
        })
    }

    pub fn len(&self) -> usize {
        self.phys_pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.phys_pages.is_empty()
    }
}

impl Deref for SharedPages {
    type Target = [PhysAddr];

    fn deref(&self) -> &Self::Target {
        &self.phys_pages
    }
}

impl Drop for SharedPages {
    fn drop(&mut self) {
        match &self.owner {
            SharedPagesOwner::Allocated => {
                for frame in &self.phys_pages {
                    dealloc_frame(*frame, self.size);
                }
            }
            SharedPagesOwner::Borrowed(_retain) => {}
        }
    }
}

// FIXME: This implementation does not allow map or unmap partial ranges.
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

    fn pages_starting_from(&self, start: VirtAddr) -> &[PhysAddr] {
        debug_assert!(start.is_aligned(self.pages.size));
        let start_index = self.page_offset + divide_page(start - self.start, self.pages.size);
        &self.pages[start_index..]
    }
}

impl BackendOps for SharedBackend {
    fn page_size(&self) -> PageSize {
        self.pages.size
    }

    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        pt: &mut PageTableCursor,
    ) -> AxResult {
        debug!("Shared::map: {:?} {:?}", range, flags);
        for (vaddr, paddr) in
            pages_in(range, self.pages.size)?.zip(self.pages_starting_from(range.start))
        {
            let newly_mapped = pt.query(vaddr).is_err();
            pt.map(vaddr, *paddr, self.pages.size, flags)?;
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
        for vaddr in pages_in(range, self.pages.size)? {
            match pt.unmap(vaddr) {
                Ok((_, _, page_size)) => {
                    debug_assert_eq!(page_size, self.pages.size);
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
            page_offset: self.page_offset + divide_page(align_diff, self.pages.size),
        }))
    }

    fn shrink_left(&mut self, shrink_size: usize) {
        self.start += shrink_size;
        self.page_offset += divide_page(shrink_size, self.pages.size);
    }

    fn shrink_right(&mut self, _shrink_size: usize) {}
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
