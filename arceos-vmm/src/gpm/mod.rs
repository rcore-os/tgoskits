use alloc::collections::BTreeMap;
use core::fmt::{Debug, Formatter, Result};

use axerrno::{AxError, AxResult};
use axhal::paging::{PageSize, PagingIfImpl};
use axvm::{AxNestedPageTable, GuestPhysAddr, HostPhysAddr};
use memory_addr::{is_aligned_4k, VirtAddr, PAGE_SIZE_4K as PAGE_SIZE};
use page_table_entry::MappingFlags;

mod arch;
pub use arch::setup_gpm;
pub use arch::GUEST_ENTRY;

type NestedPageTable = AxNestedPageTable<PagingIfImpl>;

#[derive(Debug)]
enum Mapper {
    Offset(usize),
}

#[derive(Debug)]
pub struct GuestMemoryRegion {
    pub gpa: GuestPhysAddr,
    pub hpa: HostPhysAddr,
    pub size: usize,
    pub flags: MappingFlags,
}

pub struct MapRegion {
    pub start: GuestPhysAddr,
    pub size: usize,
    pub flags: MappingFlags,
    mapper: Mapper,
}

impl MapRegion {
    pub fn new_offset(
        start_gpa: GuestPhysAddr,
        start_hpa: HostPhysAddr,
        size: usize,
        flags: MappingFlags,
    ) -> Self {
        assert!(is_aligned_4k(start_gpa));
        assert!(start_hpa.is_aligned_4k());
        assert!(is_aligned_4k(size));
        let offset = start_gpa - start_hpa.as_usize();
        Self {
            start: start_gpa,
            size,
            flags,
            mapper: Mapper::Offset(offset),
        }
    }

    fn is_overlap_with(&self, other: &Self) -> bool {
        let s0 = self.start;
        let e0 = s0 + self.size;
        let s1 = other.start;
        let e1 = s1 + other.size;
        !(e0 <= s1 || e1 <= s0)
    }

    fn target(&self, gpa: GuestPhysAddr) -> HostPhysAddr {
        match self.mapper {
            Mapper::Offset(off) => HostPhysAddr::from(gpa.wrapping_sub(off)),
        }
    }

    fn map_to(&self, npt: &mut NestedPageTable) -> AxResult {
        let mut start = self.start;
        let end = start + self.size;
        debug!("map_to() {:#x?}", self);
        while start < end {
            let target = self.target(start);
            // Here `VirtAddr` represents `GuestPhysAddr`, the physical address from the Guest's perspective.
            npt.map(VirtAddr::from(start), target, PageSize::Size4K, self.flags)
                .map_err(|err| {
                    warn!("NestedPageTable map error {:?}", err);
                    AxError::BadState
                })?;
            start += PAGE_SIZE;
        }
        Ok(())
    }

    fn unmap_to(&self, npt: &mut NestedPageTable) -> AxResult {
        let mut start = self.start;
        let end = start + self.size;
        while start < end {
            // Here `VirtAddr` represents `GuestPhysAddr`, the physical address from the Guest's perspective.
            npt.unmap(VirtAddr::from(start)).map_err(|err| {
                warn!("NestedPageTable unmap error {:?}", err);
                AxError::BadState
            })?;
            start += PAGE_SIZE;
        }
        Ok(())
    }
}

impl Debug for MapRegion {
    fn fmt(&self, f: &mut Formatter) -> Result {
        f.debug_struct("MapRegion")
            .field("range", &(self.start..self.start + self.size))
            .field("size", &self.size)
            .field("flags", &self.flags)
            .field("mapper", &self.mapper)
            .finish()
    }
}

impl From<GuestMemoryRegion> for MapRegion {
    fn from(r: GuestMemoryRegion) -> Self {
        Self::new_offset(r.gpa, r.hpa, r.size, r.flags)
    }
}

pub struct GuestPhysMemorySet {
    regions: BTreeMap<GuestPhysAddr, MapRegion>,
    npt: NestedPageTable,
}

impl GuestPhysMemorySet {
    pub fn new() -> AxResult<Self> {
        let mut npt = NestedPageTable::try_new().map_err(|err| {
            warn!("NestedPageTable try_new() get err {:?}", err);
            AxError::NoMemory
        })?;
        if usize::from(npt.root_paddr()) & (1<<12) != 0 {
            npt = NestedPageTable::try_new().map_err(|err| {
                warn!("NestedPageTable try_new() get err {:?}", err);
                AxError::NoMemory
            })?;
        }
        Ok(Self {
            npt: npt,
            regions: BTreeMap::new(),
        })
        // Ok(Self {
        //     npt: NestedPageTable::try_new().map_err(|err| {
        //         warn!("NestedPageTable try_new() get err {:?}", err);
        //         AxError::NoMemory
        //     })?,
        //     regions: BTreeMap::new(),
        // })
    }

    pub fn nest_page_table_root(&self) -> HostPhysAddr {
        self.npt.root_paddr()
    }

    fn test_free_area(&self, other: &MapRegion) -> bool {
        if let Some((_, before)) = self.regions.range(..other.start).last() {
            if before.is_overlap_with(other) {
                return false;
            }
        }
        if let Some((_, after)) = self.regions.range(other.start..).next() {
            if after.is_overlap_with(other) {
                return false;
            }
        }
        true
    }

    pub fn map_region(&mut self, region: MapRegion) -> AxResult {
        if region.size == 0 {
            return Ok(());
        }
        if !self.test_free_area(&region) {
            warn!(
                "MapRegion({:#x}..{:#x}) overlapped in:\n{:#x?}",
                region.start,
                region.start + region.size,
                self
            );
            return Err(AxError::InvalidInput);
        }
        region.map_to(&mut self.npt)?;
        self.regions.insert(region.start, region);
        Ok(())
    }

    pub fn clear(&mut self) {
        for region in self.regions.values() {
            region.unmap_to(&mut self.npt).unwrap();
        }
        self.regions.clear();
    }
}

impl Drop for GuestPhysMemorySet {
    fn drop(&mut self) {
        debug!("GuestPhysMemorySet Dropped");
        self.clear();
    }
}

impl Debug for GuestPhysMemorySet {
    fn fmt(&self, f: &mut Formatter) -> Result {
        f.debug_struct("GuestPhysMemorySet")
            .field("page_table_root", &self.nest_page_table_root())
            .field("regions", &self.regions)
            .finish()
    }
}