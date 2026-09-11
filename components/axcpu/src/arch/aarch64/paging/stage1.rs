//! AArch64 page-table descriptor format.

use core::marker::PhantomData;

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::PageTableEntry;

use super::super::mmu::{El1, El2};
use crate::paging::MappingFlags;

pub(crate) const PAGE_SIZE: usize = PAGE_SIZE_4K;
pub(crate) const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
pub(crate) const MAX_BLOCK_LEVEL: usize = 3;
pub(crate) const ADDRESS_BITS: usize =
    PAGE_SIZE.trailing_zeros() as usize + LEVEL_BITS.len() * LEVEL_BITS[0];

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    /// Stage-one descriptor attributes shared by boot and runtime mappings.
    pub struct DescriptorFlags: u64 {
        /// The descriptor is valid.
        const VALID = 1 << 0;
        /// A table descriptor, or a page at the final level.
        const NON_BLOCK = 1 << 1;
        /// MAIR attribute index field, encoded in descriptor bits [4:2].
        const ATTR_INDEX = 0b111 << 2;
        /// EL0 access is permitted in the EL1 translation regime.
        const AP_EL0 = 1 << 6;
        /// Write access is prohibited.
        const AP_RO = 1 << 7;
        /// Outer-shareable memory.
        const SH_OUTER = 0b10 << 8;
        /// Inner-shareable memory.
        const SH_INNER = 0b11 << 8;
        /// The access flag is set.
        const AF = 1 << 10;
        /// Non-global translation: the TLB entry is scoped by its ASID.
        const NON_GLOBAL = 1 << 11;
        /// Privileged execute-never in the EL1 translation regime.
        const PXN = 1 << 53;
        /// EL0 execute-never at EL1; execute-never at non-VHE EL2.
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

/// Native stage-one page-table descriptor.
pub type Pte = El1Pte;

/// EL1 stage-one descriptor, including EL0 permissions and separate PXN/UXN.
pub type El1Pte = Stage1Pte<El1>;
/// Non-VHE EL2 stage-one descriptor, with the EL2 execute-never encoding.
pub type El2Pte = Stage1Pte<El2>;

mod sealed {
    pub trait Regime {}
    impl Regime for super::El1 {}
    impl Regime for super::El2 {}
}

/// CPU-defined stage-one regimes with distinct descriptor semantics.
/// The sealed implementations prevent unsupported encodings.
pub trait Stage1Regime: sealed::Regime + Copy + Default + Send + Sync + 'static {
    /// Whether the descriptor belongs to the non-VHE EL2 regime.
    const EL2: bool;
}
impl Stage1Regime for El1 {
    const EL2: bool = false;
}
impl Stage1Regime for El2 {
    const EL2: bool = true;
}

impl DescriptorFlags {
    const ATTR_INDEX_MASK: u64 = 0x1c;

    /// Replaces the MAIR slot selector, rejecting indices outside 0..8.
    /// The selected slot's value is supplied when configuring the MMU.
    pub const fn with_attribute_index(self, index: u8) -> Option<Self> {
        if index >= 8 {
            return None;
        }
        Some(Self::from_bits_retain(
            (self.bits() & !Self::ATTR_INDEX_MASK) | ((index as u64) << 2),
        ))
    }

    /// Returns the selected MAIR slot independently of its configured value.
    pub const fn attribute_index(self) -> u8 {
        ((self.bits() & Self::ATTR_INDEX_MASK) >> 2) as u8
    }

    const fn from_mem_attr(idx: A64MemAttr) -> Self {
        let mut bits = (idx as u64) << 2;
        if matches!(idx, A64MemAttr::Normal | A64MemAttr::NormalNonCacheable) {
            bits |= Self::SH_INNER.bits();
        }
        Self::from_bits_retain(bits)
    }

    const fn mem_attr(self) -> A64MemAttr {
        let idx = (self.bits() & Self::ATTR_INDEX_MASK) >> 2;
        match idx {
            1 => A64MemAttr::Normal,
            2 => A64MemAttr::NormalNonCacheable,
            // Slots outside the native 0..3 mapping policy are not interpreted
            // as Normal. Boot owners with additional MAIR slots use their
            // own PteConfig decoder over the shared raw descriptor.
            _ => A64MemAttr::Device,
        }
    }
}

/// AArch64 VMSAv8-64 translation-table descriptor.
#[derive(Clone, Copy, Default)]
#[repr(transparent)]
pub struct Stage1Pte<R: Stage1Regime>(u64, PhantomData<R>);

impl<R: Stage1Regime> Stage1Pte<R> {
    const PHYS_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

    /// Encodes an address and stage-one attributes without installing a mapping.
    /// Installation must satisfy the chosen EL1/EL2 regime, physical-address
    /// width and page/block alignment. Low address bits are discarded.
    pub const fn from_parts(paddr: PhysAddr, flags: DescriptorFlags) -> Self {
        Self(
            (paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | flags.bits(),
            PhantomData,
        )
    }

    /// Returns attributes without interpreting the selected translation regime.
    pub fn flags(self) -> DescriptorFlags {
        // AttrIndx[2:0] occupies bits 4:2 but is decoded as a numeric field,
        // not as named bitflags. Retain those bits so querying a Normal PTE
        // cannot silently turn it into Device memory when its flags are reused.
        let attr_mask = DescriptorFlags::all().bits() | DescriptorFlags::ATTR_INDEX_MASK;
        DescriptorFlags::from_bits_retain(self.0 & attr_mask)
    }

    fn leaf_attr(config: MappingFlags) -> DescriptorFlags {
        let mem_attr = if config.contains(MappingFlags::DEVICE) {
            A64MemAttr::Device
        } else if config.contains(MappingFlags::UNCACHED) {
            A64MemAttr::NormalNonCacheable
        } else {
            A64MemAttr::Normal
        };
        let mut attr = DescriptorFlags::from_mem_attr(mem_attr) | DescriptorFlags::AF;
        if config.contains(MappingFlags::READ) {
            attr |= DescriptorFlags::VALID;
        }
        if !config.contains(MappingFlags::WRITE) {
            attr |= DescriptorFlags::AP_RO;
        }
        if !R::EL2 {
            if config.contains(MappingFlags::USER) {
                attr |=
                    DescriptorFlags::AP_EL0 | DescriptorFlags::NON_GLOBAL | DescriptorFlags::PXN;
                if !config.contains(MappingFlags::EXECUTE) {
                    attr |= DescriptorFlags::UXN;
                }
            } else {
                attr |= DescriptorFlags::UXN;
                if !config.contains(MappingFlags::EXECUTE) {
                    attr |= DescriptorFlags::PXN;
                }
            }
        }
        if R::EL2 && !config.contains(MappingFlags::EXECUTE) {
            attr |= DescriptorFlags::UXN;
        }
        attr
    }
}

impl<R: Stage1Regime> PageTableEntry for Stage1Pte<R> {
    type PteConfig = MappingFlags;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, is_huge: bool) -> Self {
        if config.is_empty() && paddr.as_usize() == 0 {
            return Self(0, PhantomData);
        }
        if config.is_empty() {
            let mut attr = DescriptorFlags::AF;
            if !is_huge {
                attr |= DescriptorFlags::NON_BLOCK;
            }
            return Self::from_parts(paddr, attr);
        }

        let mut attr = Self::leaf_attr(config);
        if !is_huge {
            attr |= DescriptorFlags::NON_BLOCK;
        }
        Self::from_parts(paddr, attr)
    }

    fn new_table(paddr: PhysAddr) -> Self {
        let attr = DescriptorFlags::NON_BLOCK | DescriptorFlags::VALID;
        Self::from_parts(paddr, attr)
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        PhysAddr::from_usize((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let attr = self.flags();
        if !attr.contains(DescriptorFlags::VALID) {
            return MappingFlags::empty();
        }
        let mut config = MappingFlags::READ;
        config.set(MappingFlags::WRITE, !attr.contains(DescriptorFlags::AP_RO));
        match attr.mem_attr() {
            A64MemAttr::Device => config |= MappingFlags::DEVICE,
            A64MemAttr::Normal => {}
            A64MemAttr::NormalNonCacheable => config |= MappingFlags::UNCACHED,
        }
        if !R::EL2 {
            let lower = attr.contains(DescriptorFlags::AP_EL0);
            config.set(MappingFlags::USER, lower);
            let executable = if lower {
                !attr.contains(DescriptorFlags::UXN)
            } else {
                !attr.contains(DescriptorFlags::PXN)
            };
            config.set(MappingFlags::EXECUTE, executable);
        }
        if R::EL2 {
            config.set(MappingFlags::EXECUTE, !attr.contains(DescriptorFlags::UXN));
        }
        config
    }

    fn present(&self) -> bool {
        self.flags().contains(DescriptorFlags::VALID)
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && !self.flags().contains(DescriptorFlags::NON_BLOCK)
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl<R: Stage1Regime> core::fmt::Debug for Stage1Pte<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("A64Pte")
            .field("raw", &self.0)
            .field("config", &self.config(false))
            .finish()
    }
}
