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

bitflags::bitflags! {
    #[derive(Debug)]
    struct PTEFlags: u64 {
        const V = 1 << 0;
        const D = 1 << 1;
        const PLVL = 1 << 2;
        const PLVH = 1 << 3;
        const MATL = 1 << 4;
        const MATH = 1 << 5;
        const GH = 1 << 6;
        const P = 1 << 7;
        const W = 1 << 8;
        const G = 1 << 12;
        const NR = 1 << 61;
        const NX = 1 << 62;
        const RPLV = 1 << 63;
    }
}

impl From<PTEFlags> for MappingFlags {
    fn from(flags: PTEFlags) -> Self {
        if !flags.contains(PTEFlags::V) {
            return Self::empty();
        }

        let mut ret = Self::empty();
        if !flags.contains(PTEFlags::NR) {
            ret |= Self::READ;
        }
        if flags.contains(PTEFlags::W) {
            ret |= Self::WRITE;
        }
        if !flags.contains(PTEFlags::NX) {
            ret |= Self::EXECUTE;
        }
        if flags.contains(PTEFlags::PLVL | PTEFlags::PLVH) {
            ret |= Self::USER;
        }
        if !flags.contains(PTEFlags::MATL) {
            if flags.contains(PTEFlags::MATH) {
                ret |= Self::UNCACHED;
            } else {
                ret |= Self::DEVICE;
            }
        }
        ret
    }
}

impl From<MappingFlags> for PTEFlags {
    fn from(flags: MappingFlags) -> Self {
        if flags.is_empty() {
            return Self::empty();
        }

        let mut ret = Self::V | Self::P;
        if !flags.contains(MappingFlags::READ) {
            ret |= Self::NR;
        }
        if flags.contains(MappingFlags::WRITE) {
            ret |= Self::W | Self::D;
        }
        if !flags.contains(MappingFlags::EXECUTE) {
            ret |= Self::NX;
        }
        if flags.contains(MappingFlags::USER) {
            ret |= Self::PLVH | Self::PLVL;
        }
        if !flags.contains(MappingFlags::DEVICE) {
            if flags.contains(MappingFlags::UNCACHED) {
                ret |= Self::MATH;
            } else {
                ret |= Self::MATL;
            }
        }
        ret
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct LoongArchPTE(u64);

impl LoongArchPTE {
    const PHYS_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;
}

impl GenericPTE for LoongArchPTE {
    fn bits(self) -> usize {
        self.0 as usize
    }

    fn new_page(paddr: HostPhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
        let mut pte_flags = PTEFlags::from(flags);
        if is_huge {
            pte_flags |= PTEFlags::GH;
        }
        Self(pte_flags.bits() | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK))
    }

    fn new_table(paddr: HostPhysAddr) -> Self {
        Self(
            (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK)
                | PTEFlags::V.bits()
                | PTEFlags::P.bits()
                | PTEFlags::MATL.bits(),
        )
    }

    fn paddr(&self) -> HostPhysAddr {
        HostPhysAddr::from((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn flags(&self) -> MappingFlags {
        PTEFlags::from_bits_truncate(self.0).into()
    }

    fn set_paddr(&mut self, paddr: HostPhysAddr) {
        self.0 =
            (self.0 & !Self::PHYS_ADDR_MASK) | (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK);
    }

    fn set_flags(&mut self, flags: MappingFlags, is_huge: bool) {
        let mut pte_flags = PTEFlags::from(flags);
        if is_huge {
            pte_flags |= PTEFlags::GH;
        }
        self.0 = (self.0 & Self::PHYS_ADDR_MASK) | pte_flags.bits();
    }

    fn is_unused(&self) -> bool {
        self.0 == 0
    }

    fn is_present(&self) -> bool {
        PTEFlags::from_bits_truncate(self.0).contains(PTEFlags::V)
    }

    fn is_huge(&self) -> bool {
        PTEFlags::from_bits_truncate(self.0).contains(PTEFlags::GH)
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
            // `invtlb 0x6/0x7` may trap on current LVZ bring-up path before the
            // guest TLB context is fully configured. Use a conservative global
            // invalidation instead. VM creation/setup is not performance critical.
            let _ = vaddr;
            asm!("invtlb 0x0, $r0, $r0");
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
