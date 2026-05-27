// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use ax_errno::{ax_err, ax_err_type};
use ax_memory_addr::PhysAddr;
use ax_memory_set::MappingError;
use ax_page_table_entry::MappingFlags;
use ax_page_table_multiarch::{PageSize, PagingHandler};

use crate::GuestPhysAddr;

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        pub type NestedPageTableL4<H> = arch::ExtendedPageTable<H>;
    } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
        /// RISC-V Level 3 nested page table (Sv39, x4 not supported)
        pub type NestedPageTableL3<H> = ax_page_table_multiarch::PageTable64<arch::Sv39MetaData<GuestPhysAddr>, arch::Rv64PTE, H>;

        /// RISC-V Level 4 nested page table (Sv48, x4 not supported)
        pub type NestedPageTableL4<H> = ax_page_table_multiarch::PageTable64<arch::Sv48MetaData<GuestPhysAddr>, arch::Rv64PTE, H>;
    } else if #[cfg(target_arch = "aarch64")] {
        /// AArch64 Level 3 nested page table type alias.
        pub type NestedPageTableL3<H> = ax_page_table_multiarch::PageTable64<arch::A64HVPagingMetaDataL3, arch::A64PTEHV, H>;

        /// AArch64 Level 4 nested page table type alias.
        pub type NestedPageTableL4<H> = ax_page_table_multiarch::PageTable64<arch::A64HVPagingMetaDataL4, arch::A64PTEHV, H>;
    } else if #[cfg(target_arch = "loongarch64")] {
        /// LoongArch Level 3 nested page table type alias.
        pub type NestedPageTableL3<H> = ax_page_table_multiarch::PageTable64<arch::LoongArchPagingMetaDataL3, arch::LoongArchPTE, H>;

        /// LoongArch Level 4 nested page table type alias.
        pub type NestedPageTableL4<H> = ax_page_table_multiarch::PageTable64<arch::LoongArchPagingMetaDataL4, arch::LoongArchPTE, H>;
    }
}

mod arch;

pub enum NestedPageTable<H: PagingHandler> {
    #[cfg(not(target_arch = "x86_64"))]
    L3(NestedPageTableL3<H>),
    L4(NestedPageTableL4<H>),
}

impl<H: PagingHandler> NestedPageTable<H> {
    pub fn new(level: usize) -> ax_errno::AxResult<Self> {
        match level {
            3 => {
                #[cfg(not(target_arch = "x86_64"))]
                {
                    let res = NestedPageTableL3::try_new().map_err(|_| ax_err_type!(NoMemory))?;
                    Ok(NestedPageTable::L3(res))
                }
                #[cfg(target_arch = "x86_64")]
                {
                    ax_err!(InvalidInput, "L3 not supported on x86_64")
                }
            }
            4 => {
                let res = NestedPageTableL4::try_new().map_err(|_| ax_err_type!(NoMemory))?;
                Ok(NestedPageTable::L4(res))
            }
            _ => ax_err!(InvalidInput, "Invalid page table level"),
        }
    }

    pub const fn root_paddr(&self) -> PhysAddr {
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
        paddr: PhysAddr,
        size: PageSize,
        flags: ax_page_table_entry::MappingFlags,
    ) -> ax_memory_set::MappingResult {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt
                .cursor()
                .map(vaddr, paddr, size, flags)
                .map_err(|_| MappingError::BadState)?,
            NestedPageTable::L4(pt) => pt
                .cursor()
                .map(vaddr, paddr, size, flags)
                .map_err(|_| MappingError::BadState)?,
        }
        Ok(())
    }

    /// Unmaps a virtual address.
    pub fn unmap(
        &mut self,
        vaddr: GuestPhysAddr,
    ) -> ax_memory_set::MappingResult<(PhysAddr, MappingFlags, PageSize)> {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt.cursor().unmap(vaddr).map_err(|_| MappingError::BadState),
            NestedPageTable::L4(pt) => pt.cursor().unmap(vaddr).map_err(|_| MappingError::BadState),
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
    ) -> ax_memory_set::MappingResult {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt
                .cursor()
                .map_region(vaddr, get_paddr, size, flags, allow_huge)
                .map_err(|_| MappingError::BadState)?,
            NestedPageTable::L4(pt) => pt
                .cursor()
                .map_region(vaddr, get_paddr, size, flags, allow_huge)
                .map_err(|_| MappingError::BadState)?,
        }
        Ok(())
    }

    /// Unmaps a region.
    pub fn unmap_region(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
    ) -> ax_memory_set::MappingResult {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt
                .cursor()
                .unmap_region(start, size)
                .map_err(|_| MappingError::BadState)?,
            NestedPageTable::L4(pt) => pt
                .cursor()
                .unmap_region(start, size)
                .map_err(|_| MappingError::BadState)?,
        }
        Ok(())
    }

    pub fn remap(&mut self, start: GuestPhysAddr, paddr: PhysAddr, flags: MappingFlags) -> bool {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt.cursor().remap(start, paddr, flags).is_ok(),
            NestedPageTable::L4(pt) => pt.cursor().remap(start, paddr, flags).is_ok(),
        }
    }

    /// Updates protection flags for a region.
    pub fn protect_region(
        &mut self,
        start: GuestPhysAddr,
        size: usize,
        new_flags: ax_page_table_entry::MappingFlags,
    ) -> bool {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt
                .cursor()
                .protect_region(start, size, new_flags) // If the TLB is refreshed immediately every time, there might be performance issues.
                .is_ok(),
            NestedPageTable::L4(pt) => pt
                .cursor()
                .protect_region(start, size, new_flags) // If the TLB is refreshed immediately every time, there might be performance issues.
                .is_ok(),
        }
    }

    /// Queries a virtual address to get physical address and mapping info.
    pub fn query(
        &self,
        vaddr: crate::GuestPhysAddr,
    ) -> ax_page_table_multiarch::PagingResult<(
        PhysAddr,
        ax_page_table_entry::MappingFlags,
        PageSize,
    )> {
        match self {
            #[cfg(not(target_arch = "x86_64"))]
            NestedPageTable::L3(pt) => pt.query(vaddr),
            NestedPageTable::L4(pt) => pt.query(vaddr),
        }
    }

    /// Translates a virtual address to a physical address.
    pub fn translate(&self, vaddr: crate::GuestPhysAddr) -> Option<crate::HostPhysAddr> {
        self.query(vaddr).ok().map(|(paddr, ..)| paddr)
    }
}
