use axerrno::{ax_err, ax_err_type};
use memory_addr::PhysAddr;
use memory_set::MappingError;
use page_table_entry::MappingFlags;
use page_table_multiarch::PagingHandler;

use crate::GuestPhysAddr;

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        pub type NestedPageTableL4<H> = arch::ExtendedPageTable<H>;

    } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
        pub type NestedPageTableL3<H> = page_table_multiarch::PageTable64<arch::Sv39MetaData<GuestPhysAddr>, arch::Rv64PTE, H>;
        pub type NestedPageTableL4<H> = page_table_multiarch::PageTable64<arch::Sv48MetaData<GuestPhysAddr>, arch::Rv64PTE, H>;

    } else if #[cfg(target_arch = "aarch64")] {
       /// AArch64 Level 3 nested page table type alias.
        pub type NestedPageTableL3<H> = page_table_multiarch::PageTable64<arch::A64HVPagingMetaDataL3, arch::A64PTEHV, H>;

        /// AArch64 Level 4 nested page table type alias.
        pub type NestedPageTableL4<H> = page_table_multiarch::PageTable64<arch::A64HVPagingMetaDataL4, arch::A64PTEHV, H>;
    }
}

mod arch;

pub enum NestedPageTable<H: PagingHandler> {
    #[cfg(not(target_arch = "x86_64"))]
    L3(NestedPageTableL3<H>),
    L4(NestedPageTableL4<H>),
}

impl<H: PagingHandler> NestedPageTable<H> {
    pub fn new(level: usize) -> axerrno::AxResult<Self> {
        match level {
            3 => {
                #[cfg(not(target_arch = "x86_64"))]
                {
                    use axerrno::ax_err_type;

                    let res = NestedPageTableL3::try_new().map_err(|_| ax_err_type!(NoMemory))?;
                    return Ok(NestedPageTable::L3(res));
                }
                #[cfg(target_arch = "x86_64")]
                {
                    return ax_err!(InvalidInput, "L3 not supported on x86_64");
                }
            }
            4 => {
                let res = NestedPageTableL4::try_new().map_err(|_| ax_err_type!(NoMemory))?;
                return Ok(NestedPageTable::L4(res));
            }
            _ => return ax_err!(InvalidInput, "Invalid page table level"),
        }
    }

    pub fn root_paddr(&self) -> memory_addr::PhysAddr {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt.root_paddr(),
            NestedPageTable::L4(pt) => pt.root_paddr(),
        }
    }

    /// Maps a virtual address to a physical address.
    pub fn map(
        &mut self,
        vaddr: crate::GuestPhysAddr,
        paddr: memory_addr::PhysAddr,
        size: page_table_multiarch::PageSize,
        flags: page_table_entry::MappingFlags,
    ) -> memory_set::MappingResult {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => {
                pt.map(vaddr, paddr, size, flags)
                    .map_err(|_| MappingError::BadState)?
                    .flush();
            }
            NestedPageTable::L4(pt) => {
                let _res = pt
                    .map(vaddr, paddr, size, flags)
                    .map_err(|_| MappingError::BadState)?
                    .flush();
            }
        }
        Ok(())
    }

    /// Unmaps a virtual address.
    pub fn unmap(
        &mut self,
        vaddr: GuestPhysAddr,
    ) -> memory_set::MappingResult<(memory_addr::PhysAddr, page_table_multiarch::PageSize)> {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => {
                let (addr, size, f) = pt.unmap(vaddr).map_err(|_| MappingError::BadState)?;
                f.flush();
                Ok((addr, size))
            }
            NestedPageTable::L4(pt) => {
                let (addr, size, f) = pt.unmap(vaddr).map_err(|_| MappingError::BadState)?;
                f.flush();
                Ok((addr, size))
            }
        }
    }

    /// Maps a region.
    pub fn map_region(
        &mut self,
        vaddr: GuestPhysAddr,
        get_paddr: impl Fn(GuestPhysAddr) -> PhysAddr,
        size: usize,
        flags: MappingFlags,
        allow_huge: bool,
        flush_tlb_by_page: bool,
    ) -> memory_set::MappingResult {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => {
                pt.map_region(vaddr, get_paddr, size, flags, allow_huge, flush_tlb_by_page)
                    .map_err(|_| MappingError::BadState)?
                    .flush_all();
            }
            NestedPageTable::L4(pt) => {
                pt.map_region(vaddr, get_paddr, size, flags, allow_huge, flush_tlb_by_page)
                    .map_err(|_| MappingError::BadState)?
                    .flush_all();
            }
        }
        Ok(())
    }

    /// Unmaps a region.
    pub fn unmap_region(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
        flush: bool,
    ) -> memory_set::MappingResult {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => {
                pt.unmap_region(start, size, flush)
                    .map_err(|_| MappingError::BadState)?
                    .ignore();
            }
            NestedPageTable::L4(pt) => {
                pt.unmap_region(start, size, flush)
                    .map_err(|_| MappingError::BadState)?
                    .ignore();
            }
        }
        Ok(())
    }

    pub fn remap(&mut self, start: GuestPhysAddr, paddr: PhysAddr, flags: MappingFlags) -> bool {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt.remap(start, paddr, flags).is_ok(),
            NestedPageTable::L4(pt) => pt.remap(start, paddr, flags).is_ok(),
        }
    }

    /// Updates protection flags for a region.
    pub fn protect_region(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
        new_flags: page_table_entry::MappingFlags,
        flush: bool,
    ) -> bool {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt
                .protect_region(start, size, new_flags, flush) // If the TLB is refreshed immediately every time, there might be performance issues.
                // The TLB refresh is managed uniformly at a higher level.
                .map(|tlb| tlb.ignore())
                .is_ok(),
            NestedPageTable::L4(pt) => pt
                .protect_region(start, size, new_flags, flush) // If the TLB is refreshed immediately every time, there might be performance issues.
                // The TLB refresh is managed uniformly at a higher level.
                .map(|tlb| tlb.ignore())
                .is_ok(),
        }
    }

    /// Queries a virtual address to get physical address and mapping info.
    pub fn query(
        &self,
        vaddr: crate::GuestPhysAddr,
    ) -> page_table_multiarch::PagingResult<(
        memory_addr::PhysAddr,
        page_table_entry::MappingFlags,
        page_table_multiarch::PageSize,
    )> {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt.query(vaddr),
            NestedPageTable::L4(pt) => pt.query(vaddr),
        }
    }

    /// Translates a virtual address to a physical address.
    pub fn translate(&self, vaddr: crate::GuestPhysAddr) -> Option<crate::HostPhysAddr> {
        self.query(vaddr).ok().map(|(paddr, _, _)| paddr)
    }
}
