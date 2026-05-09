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

use core::{arch::asm, fmt};

use ax_page_table_entry::{GenericPTE, MappingFlags};
use ax_page_table_multiarch::PagingMetaData;

use crate::{GuestPhysAddr, HostPhysAddr};

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct LoongArchPTE(u64);

impl LoongArchPTE {
    const PHYS_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

    const VALID: u64 = 1 << 0;
    const DIRTY: u64 = 1 << 1;
    const PLV_MASK: u64 = 0b11 << 3;
    const MAT_SHIFT: u64 = 5;
    const MAT_MASK: u64 = 0b11 << 5;
    const GLOBAL: u64 = 1 << 7;
    const PS_SHIFT: u64 = 8;
    const PS_MASK: u64 = 0b111 << 8;

    const MAT_STRONG_UNORDERED: u64 = 0b00 << Self::MAT_SHIFT;
    const MAT_COHERENT_CACHED: u64 = 0b01 << Self::MAT_SHIFT;
    const MAT_WEAK_UNORDERED: u64 = 0b10 << Self::MAT_SHIFT;
    const MAT_WEAK_UNORDERED_EXEC: u64 = 0b11 << Self::MAT_SHIFT;

    const PS_4K: u64 = 0b000 << Self::PS_SHIFT;
    const PS_1M: u64 = 0b100 << Self::PS_SHIFT;
}

impl GenericPTE for LoongArchPTE {
    fn bits(self) -> usize {
        self.0 as usize
    }

    fn new_page(paddr: HostPhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
        let mut pte_value = paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK;
        pte_value |= Self::VALID;
        pte_value |= Self::PLV_MASK;
        pte_value |= Self::GLOBAL;

        if flags.contains(MappingFlags::WRITE) {
            pte_value |= Self::DIRTY;
        }

        if flags.contains(MappingFlags::DEVICE) {
            pte_value |= Self::MAT_STRONG_UNORDERED;
        } else if flags.contains(MappingFlags::UNCACHED) {
            pte_value |= Self::MAT_WEAK_UNORDERED;
        } else {
            pte_value |= Self::MAT_COHERENT_CACHED;
        }

        pte_value |= if is_huge { Self::PS_1M } else { Self::PS_4K };

        if flags.contains(MappingFlags::EXECUTE)
            && (pte_value & Self::MAT_MASK) == Self::MAT_STRONG_UNORDERED
        {
            pte_value = (pte_value & !Self::MAT_MASK) | Self::MAT_WEAK_UNORDERED_EXEC;
        }

        Self(pte_value)
    }

    fn new_table(paddr: HostPhysAddr) -> Self {
        Self(
            (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK)
                | Self::VALID
                | Self::DIRTY
                | Self::PLV_MASK
                | Self::GLOBAL
                | Self::MAT_COHERENT_CACHED
                | Self::PS_4K,
        )
    }

    fn paddr(&self) -> HostPhysAddr {
        HostPhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn flags(&self) -> MappingFlags {
        let mut flags = MappingFlags::empty();

        if self.0 & Self::VALID != 0 {
            flags |= MappingFlags::READ;
        }
        if self.0 & Self::DIRTY != 0 {
            flags |= MappingFlags::WRITE;
        }

        let mat = self.0 & Self::MAT_MASK;
        if mat == Self::MAT_COHERENT_CACHED || mat == Self::MAT_WEAK_UNORDERED_EXEC {
            flags |= MappingFlags::EXECUTE;
        }
        if mat == Self::MAT_STRONG_UNORDERED {
            flags |= MappingFlags::DEVICE;
        }
        if mat == Self::MAT_WEAK_UNORDERED {
            flags |= MappingFlags::UNCACHED;
        }

        flags
    }

    fn set_paddr(&mut self, paddr: HostPhysAddr) {
        self.0 =
            (self.0 & !Self::PHYS_ADDR_MASK) | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK);
    }

    fn set_flags(&mut self, flags: MappingFlags, is_huge: bool) {
        let paddr = self.0 & Self::PHYS_ADDR_MASK;
        *self = Self::new_page(HostPhysAddr::from(paddr as usize), flags, is_huge);
    }

    fn is_unused(&self) -> bool {
        self.0 == 0
    }

    fn is_present(&self) -> bool {
        self.0 & Self::VALID != 0
    }

    fn is_huge(&self) -> bool {
        (self.0 & Self::PS_MASK) >= Self::PS_1M
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl fmt::Debug for LoongArchPTE {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoongArchPTE")
            .field("raw", &self.0)
            .field("paddr", &self.paddr())
            .field("flags", &self.flags())
            .field("is_huge", &self.is_huge())
            .finish()
    }
}

#[derive(Copy, Clone)]
pub struct LoongArchPagingMetaDataL3;

impl PagingMetaData for LoongArchPagingMetaDataL3 {
    const LEVELS: usize = 3;
    const VA_MAX_BITS: usize = 39;
    const PA_MAX_BITS: usize = 48;

    type VirtAddr = GuestPhysAddr;

    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        unsafe {
            let gstat: usize;
            asm!("csrrd {}, 0x50", out(reg) gstat);
            let gid = (gstat >> 16) & 0xff;

            if let Some(vaddr) = vaddr {
                asm!("invtlb 0x7, {0}, {1}", in(reg) gid, in(reg) vaddr.as_usize());
            } else {
                asm!("invtlb 0x6, {0}, $r0", in(reg) gid);
            }
            asm!("dbar 0");
        }
    }
}

#[derive(Copy, Clone)]
pub struct LoongArchPagingMetaDataL4;

impl PagingMetaData for LoongArchPagingMetaDataL4 {
    const LEVELS: usize = 4;
    const VA_MAX_BITS: usize = 48;
    const PA_MAX_BITS: usize = 48;

    type VirtAddr = GuestPhysAddr;

    fn flush_tlb(vaddr: Option<Self::VirtAddr>) {
        LoongArchPagingMetaDataL3::flush_tlb(vaddr);
    }
}
