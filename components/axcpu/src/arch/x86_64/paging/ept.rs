//! Intel Extended Page Table encoding, from AxVM's nested-paging backend.
//! See Intel SDM Volume 3C, section 28.3.2.

use crate::{PhysAddr, paging::PageTableEntry};

bitflags::bitflags! {
    /// Hardware flags in an Intel EPT descriptor.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct EptFlags: u64 {
        /// Reads are permitted through this entry.
        const READ = 1 << 0;
        /// Writes are permitted through this entry.
        const WRITE = 1 << 1;
        /// Supervisor execution is permitted through this entry.
        const EXECUTE = 1 << 2;
        /// Leaf memory type, independently of guest PAT selection.
        const MEM_TYPE_MASK = 7 << 3;
        /// Ignore the guest PAT when determining the effective memory type.
        const IGNORE_PAT = 1 << 6;
        /// A directory-level entry maps a large page.
        const HUGE_PAGE = 1 << 7;
        /// Hardware accessed state, when enabled in EPTP.
        const ACCESSED = 1 << 8;
        /// Hardware dirty state, when enabled in EPTP.
        const DIRTY = 1 << 9;
        /// User execution permission when mode-based execute control is enabled.
        const EXECUTE_FOR_USER = 1 << 10;
    }
}

/// Non-reserved EPT leaf memory-type encodings.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EptMemoryType {
    /// Uncacheable memory.
    Uncached       = 0,
    /// Write-combining memory.
    WriteCombining = 1,
    /// Write-through memory.
    WriteThrough   = 4,
    /// Write-protected memory.
    WriteProtected = 5,
    /// Write-back memory.
    WriteBack      = 6,
}

impl EptFlags {
    /// Replaces the memory-type field while retaining every other bit.
    pub const fn with_memory_type(self, memory_type: EptMemoryType) -> Self {
        Self::from_bits_retain(
            (self.bits() & !Self::MEM_TYPE_MASK.bits()) | ((memory_type as u64) << 3),
        )
    }

    /// Decodes a leaf memory type; reserved encodings return `None`.
    pub const fn memory_type(self) -> Option<EptMemoryType> {
        match (self.bits() & Self::MEM_TYPE_MASK.bits()) >> 3 {
            0 => Some(EptMemoryType::Uncached),
            1 => Some(EptMemoryType::WriteCombining),
            4 => Some(EptMemoryType::WriteThrough),
            5 => Some(EptMemoryType::WriteProtected),
            6 => Some(EptMemoryType::WriteBack),
            _ => None,
        }
    }
}

/// An EPT descriptor, independent of table allocation and EPTP geometry.
#[repr(transparent)]
#[derive(Clone, Copy, Default)]
pub struct EptEntry(u64);

impl EptEntry {
    const PHYS_ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

    /// Encodes a host physical page and explicit hardware flags.
    ///
    /// This does not install a translation. The table owner must validate the
    /// CPU's physical width, large-page alignment, supported memory types,
    /// permissions and optional EPT controls before making this entry live.
    pub const fn from_parts(paddr: PhysAddr, flags: EptFlags) -> Self {
        Self((paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | flags.bits())
    }

    /// Returns hardware flags without applying a VM mapping policy.
    pub fn flags(self) -> EptFlags {
        EptFlags::from_bits_truncate(self.0)
    }
}

impl PageTableEntry for EptEntry {
    type PteConfig = EptFlags;

    fn new_page(paddr: PhysAddr, mut flags: EptFlags, is_huge: bool) -> Self {
        flags.set(EptFlags::HUGE_PAGE, is_huge);
        Self::from_parts(paddr, flags)
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self::from_parts(paddr, EptFlags::READ | EptFlags::WRITE | EptFlags::EXECUTE)
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        PhysAddr::from_usize((self.0 & Self::PHYS_ADDR_MASK) as usize)
    }

    fn config(&self, _is_dir: bool) -> EptFlags {
        self.flags()
    }

    fn present(&self) -> bool {
        self.flags()
            .intersects(EptFlags::READ | EptFlags::WRITE | EptFlags::EXECUTE)
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && self.flags().contains(EptFlags::HUGE_PAGE)
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl core::fmt::Debug for EptEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EptEntry")
            .field("raw", &self.0)
            .field("paddr", &self.paddr(false))
            .field("flags", &self.flags())
            .field("memory_type", &self.flags().memory_type())
            .finish()
    }
}
