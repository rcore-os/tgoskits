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

use core::{convert::TryFrom, fmt};

use ax_page_table_entry::{GenericPTE, MappingFlags};
use ax_page_table_multiarch::{PageTable64, PagingMetaData};
use bit_field::BitField;

use crate::{GuestPhysAddr, HostPhysAddr};

bitflags::bitflags! {
    /// EPT entry flags. (SDM Vol. 3C, Section 28.3.2)
    struct EPTFlags: u64 {
        /// Read access.
        const READ =                1 << 0;
        /// Write access.
        const WRITE =               1 << 1;
        /// Execute access.
        const EXECUTE =             1 << 2;
        /// EPT memory type. Only for terminate pages.
        const MEM_TYPE_MASK =       0b111 << 3;
        /// Ignore PAT memory type. Only for terminate pages.
        const IGNORE_PAT =          1 << 6;
        /// Specifies that the entry maps a huge frame instead of a page table.
        /// Only allowed in P2 or P3 tables.
        const HUGE_PAGE =           1 << 7;
        /// If bit 6 of EPTP is 1, accessed flag for EPT.
        const ACCESSED =            1 << 8;
        /// If bit 6 of EPTP is 1, dirty flag for EPT.
        const DIRTY =               1 << 9;
        /// Execute access for user-mode linear addresses.
        const EXECUTE_FOR_USER =    1 << 10;
    }
}

numeric_enum_macro::numeric_enum! {
    #[repr(u8)]
    #[derive(Debug, PartialEq, Clone, Copy)]
    /// EPT memory typing. (SDM Vol. 3C, Section 28.3.7)
    enum EPTMemType {
        Uncached = 0,
        WriteCombining = 1,
        WriteThrough = 4,
        WriteProtected = 5,
        WriteBack = 6,
    }
}

impl EPTFlags {
    fn set_mem_type(&mut self, mem_type: EPTMemType) {
        let mut bits = self.bits();
        bits.set_bits(3..6, mem_type as u64);
        *self = Self::from_bits_truncate(bits)
    }
    fn mem_type(&self) -> Result<EPTMemType, u8> {
        EPTMemType::try_from(self.bits().get_bits(3..6) as u8)
    }
}

impl From<MappingFlags> for EPTFlags {
    fn from(f: MappingFlags) -> Self {
        if f.is_empty() {
            return Self::empty();
        }
        let mut ret = Self::empty();
        if f.contains(MappingFlags::READ) {
            ret |= Self::READ;
        }
        if f.contains(MappingFlags::WRITE) {
            ret |= Self::WRITE;
        }
        if f.contains(MappingFlags::EXECUTE) {
            ret |= Self::EXECUTE;
        }
        if !f.contains(MappingFlags::DEVICE) {
            ret.set_mem_type(EPTMemType::WriteBack);
        }
        ret
    }
}

impl From<EPTFlags> for MappingFlags {
    fn from(f: EPTFlags) -> Self {
        let mut ret = MappingFlags::empty();
        if f.contains(EPTFlags::READ) {
            ret |= Self::READ;
        }
        if f.contains(EPTFlags::WRITE) {
            ret |= Self::WRITE;
        }
        if f.contains(EPTFlags::EXECUTE) {
            ret |= Self::EXECUTE;
        }
        if let Ok(EPTMemType::Uncached) = f.mem_type() {
            ret |= Self::DEVICE;
        }
        ret
    }
}

/// An x86_64 VMX extented page table entry.
/// Note: The [EPTEntry] can be moved to the independent crate `ax-page-table-entry`.
#[derive(Clone, Copy)]
#[repr(transparent)]
#[cfg_attr(feature = "svm", allow(dead_code))]
pub struct EPTEntry(u64);

#[cfg_attr(feature = "svm", allow(dead_code))]
impl EPTEntry {
    const PHYS_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000; // bits 12..52
}

impl GenericPTE for EPTEntry {
    fn new_page(paddr: HostPhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
        let mut flags = EPTFlags::from(flags);
        if is_huge {
            flags |= EPTFlags::HUGE_PAGE;
        }
        Self(flags.bits() | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK))
    }
    fn new_table(paddr: HostPhysAddr) -> Self {
        let flags = EPTFlags::READ | EPTFlags::WRITE | EPTFlags::EXECUTE;
        Self(flags.bits() | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK))
    }
    fn paddr(&self) -> HostPhysAddr {
        HostPhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }
    fn flags(&self) -> MappingFlags {
        EPTFlags::from_bits_truncate(self.0).into()
    }
    fn set_paddr(&mut self, paddr: HostPhysAddr) {
        self.0 = (self.0 & !Self::PHYS_ADDR_MASK) | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK)
    }

    fn set_flags(&mut self, flags: MappingFlags, is_huge: bool) {
        let mut flags = EPTFlags::from(flags);
        if is_huge {
            flags |= EPTFlags::HUGE_PAGE;
        }
        self.0 = (self.0 & Self::PHYS_ADDR_MASK) | flags.bits()
    }
    fn is_unused(&self) -> bool {
        self.0 == 0
    }
    fn is_present(&self) -> bool {
        self.0 & 0x7 != 0 // RWX != 0
    }
    fn is_huge(&self) -> bool {
        EPTFlags::from_bits_truncate(self.0).contains(EPTFlags::HUGE_PAGE)
    }
    fn clear(&mut self) {
        self.0 = 0
    }

    fn bits(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Debug for EPTEntry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("EPTEntry")
            .field("raw", &self.0)
            .field("hpaddr", &self.paddr())
            .field("flags", &self.flags())
            .field("mem_type", &EPTFlags::from_bits_truncate(self.0).mem_type())
            .finish()
    }
}

bitflags::bitflags! {
    /// AMD SVM nested page table entry flags.
    struct NPTFlags: u64 {
        const PRESENT =       1 << 0;
        const WRITE =         1 << 1;
        const USER =          1 << 2;
        const WRITE_THROUGH = 1 << 3;
        const NO_CACHE =      1 << 4;
        const ACCESSED =      1 << 5;
        const DIRTY =         1 << 6;
        const HUGE_PAGE =     1 << 7;
        const GLOBAL =        1 << 8;
        const NO_EXECUTE =    1 << 63;
    }
}

impl From<MappingFlags> for NPTFlags {
    fn from(f: MappingFlags) -> Self {
        if f.is_empty() {
            return Self::empty();
        }
        let mut ret = Self::PRESENT;
        if f.contains(MappingFlags::WRITE) {
            ret |= Self::WRITE;
        }
        if f.contains(MappingFlags::USER) {
            ret |= Self::USER;
        }
        if !f.contains(MappingFlags::EXECUTE) {
            ret |= Self::NO_EXECUTE;
        }
        if f.contains(MappingFlags::DEVICE) || f.contains(MappingFlags::UNCACHED) {
            ret |= Self::NO_CACHE | Self::WRITE_THROUGH;
        }
        ret
    }
}

impl From<NPTFlags> for MappingFlags {
    fn from(f: NPTFlags) -> Self {
        if !f.contains(NPTFlags::PRESENT) {
            return Self::empty();
        }
        let mut ret = MappingFlags::READ;
        if f.contains(NPTFlags::WRITE) {
            ret |= MappingFlags::WRITE;
        }
        if f.contains(NPTFlags::USER) {
            ret |= MappingFlags::USER;
        }
        if !f.contains(NPTFlags::NO_EXECUTE) {
            ret |= MappingFlags::EXECUTE;
        }
        if f.contains(NPTFlags::NO_CACHE) {
            ret |= MappingFlags::DEVICE;
        }
        ret
    }
}

/// An AMD SVM nested page table entry.
#[derive(Clone, Copy)]
#[repr(transparent)]
#[cfg_attr(not(feature = "svm"), allow(dead_code))]
pub struct NPTEntry(u64);

#[cfg_attr(not(feature = "svm"), allow(dead_code))]
impl NPTEntry {
    const PHYS_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;
}

impl GenericPTE for NPTEntry {
    fn new_page(paddr: HostPhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
        let mut flags = NPTFlags::from(flags);
        if is_huge {
            flags |= NPTFlags::HUGE_PAGE;
        }
        Self(flags.bits() | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK))
    }

    fn new_table(paddr: HostPhysAddr) -> Self {
        let flags = NPTFlags::PRESENT | NPTFlags::WRITE | NPTFlags::USER;
        Self(flags.bits() | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK))
    }

    fn paddr(&self) -> HostPhysAddr {
        HostPhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn flags(&self) -> MappingFlags {
        NPTFlags::from_bits_truncate(self.0).into()
    }

    fn set_paddr(&mut self, paddr: HostPhysAddr) {
        self.0 = (self.0 & !Self::PHYS_ADDR_MASK) | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK)
    }

    fn set_flags(&mut self, flags: MappingFlags, is_huge: bool) {
        let mut flags = NPTFlags::from(flags);
        if is_huge {
            flags |= NPTFlags::HUGE_PAGE;
        }
        self.0 = (self.0 & Self::PHYS_ADDR_MASK) | flags.bits()
    }

    fn bits(self) -> usize {
        self.0 as usize
    }

    fn is_unused(&self) -> bool {
        self.0 == 0
    }

    fn is_present(&self) -> bool {
        NPTFlags::from_bits_truncate(self.0).contains(NPTFlags::PRESENT)
    }

    fn is_huge(&self) -> bool {
        NPTFlags::from_bits_truncate(self.0).contains(NPTFlags::HUGE_PAGE)
    }

    fn clear(&mut self) {
        self.0 = 0
    }
}

impl fmt::Debug for NPTEntry {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("NPTEntry")
            .field("raw", &self.0)
            .field("hpaddr", &self.paddr())
            .field("flags", &self.flags())
            .finish()
    }
}

/// Metadata of VMX extended page tables.
pub struct ExtendedPageTableMetadata;

impl PagingMetaData for ExtendedPageTableMetadata {
    const LEVELS: usize = 4;
    const PA_MAX_BITS: usize = 52;
    const VA_MAX_BITS: usize = 48;

    type VirtAddr = GuestPhysAddr;

    // Under the x86 architecture, flushing the TLB requires privileged
    // instructions. Hosted binaries such as integration tests run in ring 3,
    // so issue TLB invalidations only for bare-metal targets.
    #[allow(unused_variables)]
    fn flush_tlb(vaddr: Option<GuestPhysAddr>) {
        #[cfg(target_os = "none")]
        {
            if let Some(vaddr) = vaddr {
                unsafe { x86::tlb::flush(vaddr.into()) }
            } else {
                unsafe { x86::tlb::flush_all() }
            }
        }
    }
}

/// The VMX extended page table. (SDM Vol. 3C, Section 29.3)
#[cfg(not(feature = "svm"))]
pub type ExtendedPageTable<H> = PageTable64<ExtendedPageTableMetadata, EPTEntry, H>;

/// The AMD SVM nested page table.
#[cfg(feature = "svm")]
pub type ExtendedPageTable<H> = PageTable64<ExtendedPageTableMetadata, NPTEntry, H>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npt_entry_encodes_regular_x86_page_flags() {
        let entry = NPTEntry::new_page(
            HostPhysAddr::from(0x2000),
            MappingFlags::READ | MappingFlags::WRITE,
            false,
        );

        assert!(entry.is_present());
        assert_eq!(entry.paddr(), HostPhysAddr::from(0x2000));
        assert!(entry.flags().contains(MappingFlags::READ));
        assert!(entry.flags().contains(MappingFlags::WRITE));
        assert!(!entry.flags().contains(MappingFlags::EXECUTE));
        assert_ne!(entry.bits() as u64 & NPTFlags::NO_EXECUTE.bits(), 0);
    }

    #[test]
    fn npt_entry_marks_huge_and_uncached_pages() {
        let entry = NPTEntry::new_page(
            HostPhysAddr::from(0x20_0000),
            MappingFlags::READ | MappingFlags::EXECUTE | MappingFlags::DEVICE,
            true,
        );

        assert!(entry.is_huge());
        assert!(entry.flags().contains(MappingFlags::DEVICE));
        assert!(entry.flags().contains(MappingFlags::EXECUTE));
        assert_eq!(
            entry.bits() as u64 & NPTFlags::HUGE_PAGE.bits(),
            NPTFlags::HUGE_PAGE.bits()
        );
        assert_eq!(
            entry.bits() as u64 & NPTFlags::NO_CACHE.bits(),
            NPTFlags::NO_CACHE.bits()
        );
    }
}
