use alloc::{sync::Arc, vec::Vec};
use core::{any::Any, ops::Deref};

use ax_memory_addr::{MemoryAddr, PhysAddr, VirtAddr, VirtAddrRange};
use ax_runtime::hal::paging::{MappingFlags, PageTable, PagingError};

use super::{
    Backend, BackendOps, CloneMapContext, MemoryAccounting, RssKind, TlbGather, alloc_frame,
    dealloc_frame, divide_page, pages_in,
};
use crate::{StarryError, StarryResult};

enum SharedPagesOwner {
    Allocated,
    Borrowed(Option<Arc<dyn Any + Send + Sync>>),
}

pub struct SharedPages {
    phys_pages: Vec<PhysAddr>,
    pub size: usize,
    owner: SharedPagesOwner,
}
impl SharedPages {
    pub fn new(size: usize, page_size: usize) -> StarryResult<Self> {
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
        page_size: usize,
        retain: Option<Arc<dyn Any + Send + Sync>>,
    ) -> StarryResult<Self> {
        if phys_pages.is_empty() {
            return Err(crate::StarryError::InvalidInput);
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

    fn pages_starting_from(&self, start: VirtAddr) -> Option<&[PhysAddr]> {
        debug_assert!(start.is_aligned(self.pages.size));
        let start_index = self.page_offset + divide_page(start - self.start, self.pages.size);
        self.pages.get(start_index..)
    }

    fn pages_for_range(&self, range: VirtAddrRange) -> StarryResult<&[PhysAddr]> {
        if range.start < self.start {
            return Err(StarryError::BadState);
        }
        let count = pages_in(range, self.pages.size)?.count();
        self.pages_starting_from(range.start)
            .and_then(|pages| pages.get(..count))
            .ok_or(StarryError::BadState)
    }

    fn validate_map(&self, range: VirtAddrRange, pt: &PageTable) -> StarryResult {
        self.pages_for_range(range)?;
        for vaddr in pages_in(range, self.pages.size)? {
            match pt.query_occupied(vaddr) {
                Ok((paddr, _, _)) => {
                    return Err(PagingError::mapping_conflict(vaddr, paddr).into());
                }
                Err(PagingError::NotMapped) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn rollback_mapped_prefix(
        &self,
        start: VirtAddr,
        mapped_size: usize,
        acct: Option<&MemoryAccounting>,
        gather: &mut TlbGather,
        pt: &mut PageTable,
    ) {
        if mapped_size == 0 {
            return;
        }

        gather.retain_backend(Backend::Shared(self.clone()));
        let range = VirtAddrRange::from_start_size(start, mapped_size);
        for vaddr in pages_in(range, self.pages.size)
            .expect("a mapped shared prefix must remain page aligned")
        {
            let (_, _, _, deferred_page_tables) = pt
                .unmap_page_deferred(vaddr)
                .expect("a shared page installed by this transaction must remain occupied");
            gather.defer_page_tables(deferred_page_tables);
            if let Some(acct) = acct {
                acct.dec(RssKind::Shmem, 1);
            }
        }
        gather.record_range(range);
    }
}

impl BackendOps for SharedBackend {
    fn page_size(&self) -> usize {
        self.pages.size
    }

    fn map(
        &self,
        range: VirtAddrRange,
        flags: MappingFlags,
        acct: Option<&MemoryAccounting>,
        gather: &mut TlbGather,
        pt: &mut PageTable,
    ) -> StarryResult {
        debug!("Shared::map: {:?} {:?}", range, flags);

        self.validate_map(range, pt)?;
        gather
            .prepare_ranges(1)
            .map_err(|_| StarryError::NoMemory)?;
        gather
            .prepare_backend_retention(1)
            .map_err(|_| StarryError::NoMemory)?;
        let physical_pages = self.pages_for_range(range)?;
        gather
            .prepare_page_table_reclaims(physical_pages.len())
            .map_err(|_| StarryError::NoMemory)?;

        let mut mapped_size = 0;
        for (vaddr, paddr) in pages_in(range, self.pages.size)?.zip(physical_pages) {
            if let Err(error) = pt.map_page(vaddr, *paddr, self.pages.size, flags) {
                self.rollback_mapped_prefix(range.start, mapped_size, acct, gather, pt);
                return Err(error.into());
            }
            mapped_size += self.pages.size;
            if let Some(acct) = acct {
                acct.inc(RssKind::Shmem, 1);
            }
        }
        Ok(())
    }

    fn unmap(
        &self,
        range: VirtAddrRange,
        acct: Option<&MemoryAccounting>,
        gather: &mut TlbGather,
        pt: &mut PageTable,
    ) -> StarryResult {
        debug!("Shared::unmap: {:?}", range);
        let mut mapped = Vec::new();
        let expected_pages = self.pages_for_range(range)?;
        for (vaddr, expected) in pages_in(range, self.pages.size)?.zip(expected_pages) {
            match pt.query_occupied(vaddr) {
                Ok((paddr, _, page_size))
                    if page_size == self.pages.size && paddr == *expected =>
                {
                    mapped.push(vaddr)
                }
                Ok(_) => return Err(StarryError::BadState),
                Err(PagingError::NotMapped) => {}
                Err(err) => return Err(err.into()),
            }
        }
        gather
            .prepare_page_table_reclaims(mapped.len())
            .map_err(|_| StarryError::NoMemory)?;
        for vaddr in mapped {
            let (_, _, _, deferred_page_tables) = pt
                .unmap_page_deferred(vaddr)
                .expect("a preflighted shared page must remain mapped under the address-space lock");
            gather.defer_page_tables(deferred_page_tables);
            if let Some(acct) = acct {
                acct.dec(RssKind::Shmem, 1);
            }
        }
        Ok(())
    }

    fn clone_map(
        &self,
        _range: VirtAddrRange,
        _flags: MappingFlags,
        _context: CloneMapContext<'_>,
    ) -> StarryResult<Backend> {
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

#[cfg(all(test, not(axtest)))]
mod tests {
    use alloc::sync::Arc;

    use ax_memory_addr::{PhysAddr, VirtAddr, VirtAddrRange};

    use super::{SharedBackend, SharedPages};
    use crate::StarryError;

    #[test]
    fn shared_mapping_rejects_a_range_larger_than_its_backing() {
        let start = VirtAddr::from_usize(0x4000_0000);
        let pages = Arc::new(
            SharedPages::borrowed(
                alloc::vec![PhysAddr::from_usize(0x8000_0000)],
                0x1000,
                None,
            )
                .unwrap(),
        );
        let backend = SharedBackend {
            start,
            pages,
            page_offset: 0,
        };

        assert!(matches!(
            backend.pages_for_range(VirtAddrRange::from_start_size(start, 0x2000)),
            Err(StarryError::BadState)
        ));
    }
}
