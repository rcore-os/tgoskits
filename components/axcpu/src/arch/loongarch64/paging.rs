//! LoongArch64 page-table entry format.

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::PageTableEntry;

use crate::paging::MappingFlags;

pub(crate) const PAGE_SIZE: usize = PAGE_SIZE_4K;
pub(crate) const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
pub(crate) const MAX_BLOCK_LEVEL: usize = 3;

/// PWCL fields matching the runtime page-table geometry.
pub(super) const PWCL_VALUE: u32 = 12 | (9 << 5) | (21 << 10) | (9 << 15) | (30 << 20) | (9 << 25);
/// PWCH fields matching the runtime page-table geometry.
pub(super) const PWCH_VALUE: u32 = 39 | (9 << 6);

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug)]
    /// LoongArch leaf attributes and software walker markers.
    pub struct DescriptorFlags: u64 {
        /// The translation is valid.
        const V = 1 << 0;
        /// The leaf is dirty.
        const D = 1 << 1;
        /// Low privilege-level bit.
        const PLVL = 1 << 2;
        /// High privilege-level bit.
        const PLVH = 1 << 3;
        /// Low memory-access-type bit; alone selects coherent cached memory.
        const MATL = 1 << 4;
        /// High memory-access-type bit; alone selects weakly ordered uncached memory.
        const MATH = 1 << 5;
        /// Global at the final level; huge-page marker at directory levels.
        const GH = 1 << 6;
        /// Software present marker.
        const P = 1 << 7;
        /// Software writable marker.
        const W = 1 << 8;
        /// Global for a huge leaf; part of the address in a base-page leaf.
        const G = 1 << 12;
        /// Read access is prohibited.
        const NR = 1 << 61;
        /// Instruction fetch is prohibited.
        const NX = 1 << 62;
    }
}

/// LoongArch64 page-table entry.
#[derive(Clone, Copy, Default)]
#[repr(transparent)]
pub struct La64Pte(u64);

/// Native stage-one page-table descriptor.
pub type Pte = La64Pte;

impl La64Pte {
    const PHYS_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

    /// Encodes a physical page base and explicit flags without mapping memory.
    /// The current 64-bit walker uses address bits 12..48. Callers installing
    /// an entry must enforce page/block alignment and the CPU's physical width.
    pub const fn from_parts(paddr: PhysAddr, flags: DescriptorFlags) -> Self {
        Self((paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | flags.bits())
    }

    /// Returns attributes without deciding whether this is a base or huge leaf.
    /// The `G` bit overlaps the base-page physical address; interpret it only
    /// after establishing the entry's level and huge-page state.
    pub fn flags(self) -> DescriptorFlags {
        DescriptorFlags::from_bits_truncate(self.0)
    }

    fn paddr(self) -> PhysAddr {
        PhysAddr::from_usize((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn leaf_flags(config: MappingFlags, is_huge: bool) -> DescriptorFlags {
        if config.is_empty() {
            return if is_huge {
                DescriptorFlags::GH
            } else {
                // Keep a non-present leaf distinct from an address-only directory entry.
                DescriptorFlags::P
            };
        }
        let mut flags = DescriptorFlags::V | DescriptorFlags::P;
        if !config.contains(MappingFlags::READ) {
            flags |= DescriptorFlags::NR;
        }
        if config.contains(MappingFlags::WRITE) {
            flags |= DescriptorFlags::W | DescriptorFlags::D;
        }
        if !config.contains(MappingFlags::EXECUTE) {
            flags |= DescriptorFlags::NX;
        }
        if config.contains(MappingFlags::USER) {
            flags |= DescriptorFlags::PLVL | DescriptorFlags::PLVH;
        }
        if config.contains(MappingFlags::UNCACHED) {
            flags |= DescriptorFlags::MATH;
        } else if !config.contains(MappingFlags::DEVICE) {
            flags |= DescriptorFlags::MATL;
        }
        let global = !config.contains(MappingFlags::USER);
        if is_huge {
            flags |= DescriptorFlags::GH;
            if global {
                flags |= DescriptorFlags::G;
            }
        } else if global {
            flags |= DescriptorFlags::GH;
        }
        flags
    }

    fn is_table(self) -> bool {
        self.paddr().as_usize() != 0 && (self.0 & !Self::PHYS_ADDR_MASK) == 0
    }
}

impl PageTableEntry for La64Pte {
    type PteConfig = MappingFlags;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, is_huge: bool) -> Self {
        if config.is_empty() && paddr.as_usize() == 0 {
            return Self(0);
        }
        Self::from_parts(paddr, Self::leaf_flags(config, is_huge))
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self::from_parts(paddr, DescriptorFlags::empty())
    }

    fn paddr(&self, is_dir: bool) -> PhysAddr {
        let flags = self.flags();
        let huge = is_dir && flags.contains(DescriptorFlags::GH);
        if huge {
            PhysAddr::from_usize(
                La64Pte::paddr(*self).as_usize() & !(DescriptorFlags::G.bits() as usize),
            )
        } else {
            La64Pte::paddr(*self)
        }
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let flags = self.flags();
        if !flags.contains(DescriptorFlags::V) {
            return MappingFlags::empty();
        }
        let mut config = MappingFlags::empty();
        config.set(MappingFlags::READ, !flags.contains(DescriptorFlags::NR));
        config.set(MappingFlags::WRITE, flags.contains(DescriptorFlags::W));
        config.set(MappingFlags::EXECUTE, !flags.contains(DescriptorFlags::NX));
        config.set(
            MappingFlags::USER,
            flags.contains(DescriptorFlags::PLVL | DescriptorFlags::PLVH),
        );
        if !flags.contains(DescriptorFlags::MATL) {
            config |= if flags.contains(DescriptorFlags::MATH) {
                MappingFlags::UNCACHED
            } else {
                MappingFlags::DEVICE
            };
        }
        config
    }

    fn present(&self) -> bool {
        self.flags().contains(DescriptorFlags::V) || self.is_table()
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && self.flags().contains(DescriptorFlags::GH)
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl core::fmt::Debug for La64Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("La64Pte")
            .field("raw", &self.0)
            .field("config", &self.config(false))
            .finish()
    }
}
