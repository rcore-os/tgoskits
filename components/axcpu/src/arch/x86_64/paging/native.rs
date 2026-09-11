//! x86_64 page-table entry format.

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::PageTableEntry;

use crate::paging::MappingFlags;

pub(crate) const PAGE_SIZE: usize = PAGE_SIZE_4K;
pub(crate) const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
pub(crate) const MAX_BLOCK_LEVEL: usize = 3;

bitflags::bitflags! {
    /// Hardware flags shared by boot and runtime x86 paging descriptors.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct DescriptorFlags: u64 {
        /// The descriptor participates in address translation.
        const PRESENT = 1 << 0;
        /// Writes are permitted by this level.
        const WRITABLE = 1 << 1;
        /// User accesses are permitted by this level.
        const USER = 1 << 2;
        /// Page-level write-through cache selection.
        const WRITE_THROUGH = 1 << 3;
        /// Page-level cache-disable selection.
        const NO_CACHE = 1 << 4;
        /// The processor has accessed this entry.
        const ACCESSED = 1 << 5;
        /// The processor has written this leaf.
        const DIRTY = 1 << 6;
        /// A directory-level entry maps a large page.
        const HUGE_PAGE = 1 << 7;
        /// The leaf translation is global when CR4.PGE is enabled.
        const GLOBAL = 1 << 8;
        /// Instruction fetch is prohibited when EFER.NXE is enabled.
        const NO_EXECUTE = 1 << 63;
    }
}

/// x86_64 page-table entry.
#[derive(Clone, Copy, Default)]
#[repr(transparent)]
pub struct X64Pte(u64);

/// Native stage-one page-table descriptor.
pub type Pte = X64Pte;

impl X64Pte {
    const PHYS_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

    /// Encodes a physical address and explicit native descriptor flags.
    ///
    /// The caller chooses leaf/directory permissions and accessed/dirty policy.
    /// Installation must satisfy the CPU's physical-address width, page-size
    /// alignment and enabled paging features. This constructor does not install
    /// the descriptor or allocate table memory.
    pub const fn from_parts(paddr: PhysAddr, flags: DescriptorFlags) -> Self {
        Self((paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | flags.bits())
    }

    /// Returns native flags without interpreting a boot or runtime policy.
    pub fn flags(self) -> DescriptorFlags {
        DescriptorFlags::from_bits_truncate(self.0)
    }
}

impl PageTableEntry for X64Pte {
    type PteConfig = MappingFlags;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, is_huge: bool) -> Self {
        if config.is_empty() && paddr.as_usize() == 0 {
            return Self(0);
        }
        if config.is_empty() {
            let huge = if is_huge {
                DescriptorFlags::HUGE_PAGE.bits()
            } else {
                0
            };
            return Self((paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | huge);
        }

        let mut flags = DescriptorFlags::PRESENT;
        if config.contains(MappingFlags::WRITE) {
            flags |= DescriptorFlags::WRITABLE;
        }
        if config.contains(MappingFlags::USER) {
            flags |= DescriptorFlags::USER;
        }
        if config.intersects(MappingFlags::DEVICE | MappingFlags::UNCACHED) {
            flags |= DescriptorFlags::NO_CACHE | DescriptorFlags::WRITE_THROUGH;
        }
        if !config.contains(MappingFlags::EXECUTE) {
            flags |= DescriptorFlags::NO_EXECUTE;
        }
        if is_huge {
            flags |= DescriptorFlags::HUGE_PAGE;
        }
        Self::from_parts(paddr, flags)
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self::from_parts(
            paddr,
            DescriptorFlags::PRESENT | DescriptorFlags::WRITABLE | DescriptorFlags::USER,
        )
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        PhysAddr::from_usize((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let flags = self.flags();
        if !flags.contains(DescriptorFlags::PRESENT) {
            return MappingFlags::empty();
        }
        let mut config = MappingFlags::READ;
        config.set(
            MappingFlags::WRITE,
            flags.contains(DescriptorFlags::WRITABLE),
        );
        config.set(
            MappingFlags::EXECUTE,
            !flags.contains(DescriptorFlags::NO_EXECUTE),
        );
        config.set(MappingFlags::USER, flags.contains(DescriptorFlags::USER));
        config.set(
            MappingFlags::UNCACHED,
            flags.contains(DescriptorFlags::NO_CACHE),
        );
        config
    }

    fn present(&self) -> bool {
        self.flags().contains(DescriptorFlags::PRESENT)
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && self.flags().contains(DescriptorFlags::HUGE_PAGE)
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl core::fmt::Debug for X64Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("X64Pte")
            .field("raw", &self.0)
            .field("config", &self.config(false))
            .finish()
    }
}
