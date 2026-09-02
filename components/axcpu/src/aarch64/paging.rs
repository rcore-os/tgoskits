//! AArch64 page-table descriptor format.

use aarch64_cpu::registers::MAIR_EL1;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::PageTableEntry;

use crate::paging::MappingFlags;

pub(crate) const PAGE_SIZE: usize = PAGE_SIZE_4K;
pub(crate) const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
pub(crate) const MAX_BLOCK_LEVEL: usize = 3;
pub(super) const ADDRESS_BITS: usize =
    PAGE_SIZE.trailing_zeros() as usize + LEVEL_BITS.len() * LEVEL_BITS[0];

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    struct A64DescriptorAttr: u64 {
        const VALID = 1 << 0;
        const NON_BLOCK = 1 << 1;
        const AP_EL0 = 1 << 6;
        const AP_RO = 1 << 7;
        const INNER = 1 << 8;
        const SHAREABLE = 1 << 9;
        const AF = 1 << 10;
        const PXN = 1 << 53;
        const UXN = 1 << 54;
    }
}

#[repr(u64)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum A64MemAttr {
    Device             = 0,
    Normal             = 1,
    NormalNonCacheable = 2,
}

#[allow(clippy::unusual_byte_groupings)]
pub(super) const MAIR_VALUE: u64 = {
    let device_n_gn_re = MAIR_EL1::Attr0_Device::nonGathering_nonReordering_EarlyWriteAck.value;
    let normal = MAIR_EL1::Attr1_Normal_Inner::WriteBack_NonTransient_ReadWriteAlloc.value
        | MAIR_EL1::Attr1_Normal_Outer::WriteBack_NonTransient_ReadWriteAlloc.value;
    let normal_non_cacheable = MAIR_EL1::Attr2_Normal_Inner::NonCacheable.value
        + MAIR_EL1::Attr2_Normal_Outer::NonCacheable.value;
    device_n_gn_re | normal | normal_non_cacheable
};

impl A64DescriptorAttr {
    const ATTR_INDEX_MASK: u64 = 0x1c;

    const fn from_mem_attr(idx: A64MemAttr) -> Self {
        let mut bits = (idx as u64) << 2;
        if matches!(idx, A64MemAttr::Normal | A64MemAttr::NormalNonCacheable) {
            bits |= Self::INNER.bits() | Self::SHAREABLE.bits();
        }
        Self::from_bits_retain(bits)
    }

    const fn mem_attr(self) -> A64MemAttr {
        let idx = (self.bits() & Self::ATTR_INDEX_MASK) >> 2;
        match idx {
            1 => A64MemAttr::Normal,
            2 => A64MemAttr::NormalNonCacheable,
            // MAIR slots 3..7 are left at zero by `MAIR_VALUE`, which is a
            // Device-nGnRnE encoding. Decode them conservatively as Device
            // instead of silently treating an unsupported index as Normal.
            _ => A64MemAttr::Device,
        }
    }
}

/// AArch64 VMSAv8-64 translation-table descriptor.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct A64Pte(u64);

impl A64Pte {
    const PHYS_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

    #[cfg(test)]
    pub(crate) fn with_attr_index(mut self, index: u64) -> Self {
        assert!(index < 8);
        self.0 &= !A64DescriptorAttr::ATTR_INDEX_MASK;
        self.0 |= index << 2;
        self
    }

    fn attr(self) -> A64DescriptorAttr {
        // AttrIndx[2:0] occupies bits 4:2 but is decoded as a numeric field,
        // not as named bitflags. Retain those bits so querying a Normal PTE
        // cannot silently turn it into Device memory when its flags are reused.
        let attr_mask = A64DescriptorAttr::all().bits() | A64DescriptorAttr::ATTR_INDEX_MASK;
        A64DescriptorAttr::from_bits_retain(self.0 & attr_mask)
    }

    fn leaf_attr(config: MappingFlags) -> A64DescriptorAttr {
        let mem_attr = if config.contains(MappingFlags::DEVICE) {
            A64MemAttr::Device
        } else if config.contains(MappingFlags::UNCACHED) {
            A64MemAttr::NormalNonCacheable
        } else {
            A64MemAttr::Normal
        };
        let mut attr = A64DescriptorAttr::from_mem_attr(mem_attr) | A64DescriptorAttr::AF;
        if config.contains(MappingFlags::READ) {
            attr |= A64DescriptorAttr::VALID;
        }
        if !config.contains(MappingFlags::WRITE) {
            attr |= A64DescriptorAttr::AP_RO;
        }
        #[cfg(not(feature = "arm-el2"))]
        {
            if config.contains(MappingFlags::USER) {
                attr |= A64DescriptorAttr::AP_EL0 | A64DescriptorAttr::PXN;
                if !config.contains(MappingFlags::EXECUTE) {
                    attr |= A64DescriptorAttr::UXN;
                }
            } else {
                attr |= A64DescriptorAttr::UXN;
                if !config.contains(MappingFlags::EXECUTE) {
                    attr |= A64DescriptorAttr::PXN;
                }
            }
        }
        #[cfg(feature = "arm-el2")]
        {
            if !config.contains(MappingFlags::EXECUTE) {
                attr |= A64DescriptorAttr::UXN;
            }
        }
        attr
    }
}

impl PageTableEntry for A64Pte {
    type PteConfig = MappingFlags;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, is_huge: bool) -> Self {
        if config.is_empty() && paddr.as_usize() == 0 {
            return Self(0);
        }
        if config.is_empty() {
            let mut attr = A64DescriptorAttr::AF;
            if !is_huge {
                attr |= A64DescriptorAttr::NON_BLOCK;
            }
            return Self((paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | attr.bits());
        }

        let mut attr = Self::leaf_attr(config);
        if !is_huge {
            attr |= A64DescriptorAttr::NON_BLOCK;
        }
        Self((paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | attr.bits())
    }

    fn new_table(paddr: PhysAddr) -> Self {
        let attr = A64DescriptorAttr::NON_BLOCK | A64DescriptorAttr::VALID;
        Self((paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | attr.bits())
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        PhysAddr::from_usize((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let attr = self.attr();
        if !attr.contains(A64DescriptorAttr::VALID) {
            return MappingFlags::empty();
        }
        let mut config = MappingFlags::READ;
        config.set(
            MappingFlags::WRITE,
            !attr.contains(A64DescriptorAttr::AP_RO),
        );
        match attr.mem_attr() {
            A64MemAttr::Device => config |= MappingFlags::DEVICE,
            A64MemAttr::Normal => {}
            A64MemAttr::NormalNonCacheable => config |= MappingFlags::UNCACHED,
        }
        #[cfg(not(feature = "arm-el2"))]
        {
            let lower = attr.contains(A64DescriptorAttr::AP_EL0);
            config.set(MappingFlags::USER, lower);
            let executable = if lower {
                !attr.contains(A64DescriptorAttr::UXN)
            } else {
                !attr.contains(A64DescriptorAttr::PXN)
            };
            config.set(MappingFlags::EXECUTE, executable);
        }
        #[cfg(feature = "arm-el2")]
        {
            config.set(
                MappingFlags::EXECUTE,
                !attr.contains(A64DescriptorAttr::UXN),
            );
        }
        config
    }

    fn present(&self) -> bool {
        self.attr().contains(A64DescriptorAttr::VALID)
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && !self.attr().contains(A64DescriptorAttr::NON_BLOCK)
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl core::fmt::Debug for A64Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("A64Pte")
            .field("raw", &self.0)
            .field("config", &self.config(false))
            .finish()
    }
}
