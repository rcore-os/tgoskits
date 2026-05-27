//! RISC-V page table entries.

use core::fmt;

use ax_memory_addr::PhysAddr;

use crate::{GenericPTE, MappingFlags};

bitflags::bitflags! {
    /// Page-table entry flags.
    #[derive(Debug)]
    pub struct PTEFlags: usize {
        /// Whether the PTE is valid.
        const V =   1 << 0;
        /// Whether the page is readable.
        const R =   1 << 1;
        /// Whether the page is writable.
        const W =   1 << 2;
        /// Whether the page is executable.
        const X =   1 << 3;
        /// Whether the page is accessible to user mode.
        const U =   1 << 4;
        /// Designates a global mapping.
        const G =   1 << 5;
        /// Indicates the virtual page has been read, written, or fetched from since the last time the A bit was cleared.
        /// When A is set 1, indicates the virtual page accessable. When A is set 0, accessing causes a page fault.
        const A =   1 << 6;
        /// Indicates the virtual page has been written since the last time the D bit was cleared.
        /// When D is set 0, writing will cause a Page Fault (Store).
        const D =   1 << 7;

        /// CPU T-Head XUANTIE-C9xx extended flags
        /// Reference datasheet:
        /// https://github.com/XUANTIE-RV/openc910/blob/main/doc/%E7%8E%84%E9%93%81C910%E7%94%A8%E6%88%B7%E6%89%8B%E5%86%8C_20240627.pdf
        ///
        #[cfg(feature = "xuantie-c9xx")]
        /// Trustable
        const SEC =   1 << 59;
        #[cfg(feature = "xuantie-c9xx")]
        /// Shareable
        const  SH =   1 << 60;
        #[cfg(feature = "xuantie-c9xx")]
        /// Bufferable
        const   B =   1 << 61;
        #[cfg(feature = "xuantie-c9xx")]
        /// Cacheable
        const   C =   1 << 62;
        #[cfg(feature = "xuantie-c9xx")]
        /// Strong order (Device)
        const  SO =   1 << 63;

    }
}

impl From<PTEFlags> for MappingFlags {
    fn from(f: PTEFlags) -> Self {
        let mut ret = Self::empty();
        if !f.contains(PTEFlags::V) {
            return ret;
        }
        if f.contains(PTEFlags::R) {
            ret |= Self::READ;
        }
        if f.contains(PTEFlags::W) {
            ret |= Self::WRITE;
        }
        if f.contains(PTEFlags::X) {
            ret |= Self::EXECUTE;
        }
        if f.contains(PTEFlags::U) {
            ret |= Self::USER;
        }
        ret
    }
}

impl From<MappingFlags> for PTEFlags {
    fn from(f: MappingFlags) -> Self {
        if f.is_empty() {
            return Self::empty();
        }
        let mut ret = Self::V;
        if f.contains(MappingFlags::READ) {
            ret |= Self::R;
        }
        if f.contains(MappingFlags::WRITE) {
            ret |= Self::W;
        }
        if f.contains(MappingFlags::EXECUTE) {
            ret |= Self::X;
        }
        if f.contains(MappingFlags::USER) {
            ret |= Self::U;
        }
        ret
    }
}

/// Sv39 and Sv48 page table entry for RV64 systems.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Rv64PTE(u64);

impl Rv64PTE {
    // bits 10..54
    const PHYS_ADDR_MASK: u64 = (1 << 54) - (1 << 10);

    /// Creates an empty descriptor with all bits set to zero.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Set CPU PTE extension flags
    /// mflags is current PTE MappingFlags
    /// extended_flags are all the extended flag bits that need to be set
    #[allow(unused)]
    pub fn set_extended_flags(&mut self, mflags: MappingFlags, extended_flags: u64) -> PTEFlags {
        #[cfg(feature = "xuantie-c9xx")]
        {
            // CPU T-Head XUANTIE-C9xx extended flags:
            if mflags.contains(MappingFlags::DEVICE) {
                self.0 |= (PTEFlags::SH | PTEFlags::SO).bits() as u64;
            } else if mflags.contains(MappingFlags::UNCACHED) {
                self.0 |= (PTEFlags::SH | PTEFlags::B).bits() as u64;
            } else {
                self.0 |= (PTEFlags::SH | PTEFlags::B | PTEFlags::C).bits() as u64;
            }
        }
        self.0 |= (extended_flags & !Self::PHYS_ADDR_MASK);
        PTEFlags::from_bits_truncate(self.0 as usize)
    }
}

impl GenericPTE for Rv64PTE {
    fn new_page(paddr: PhysAddr, mflags: MappingFlags, _is_huge: bool) -> Self {
        let mut page = Self((paddr.as_usize() >> 2) as u64 & Self::PHYS_ADDR_MASK);
        page.set_flags(mflags, _is_huge);
        page
    }

    fn new_table(paddr: PhysAddr) -> Self {
        let mut table = Self::new_page(paddr, MappingFlags::empty(), false);
        // Default table flags: PTEFlags::V
        table.0 = (table.0 & !0x3ff) | PTEFlags::V.bits() as u64;
        table
    }

    fn paddr(&self) -> PhysAddr {
        PhysAddr::from(((self.0 & Self::PHYS_ADDR_MASK) << 2) as usize)
    }

    fn flags(&self) -> MappingFlags {
        PTEFlags::from_bits_truncate(self.0 as usize).into()
    }

    fn set_paddr(&mut self, paddr: PhysAddr) {
        self.0 = (self.0 & !Self::PHYS_ADDR_MASK)
            | ((paddr.as_usize() as u64 >> 2) & Self::PHYS_ADDR_MASK);
    }

    fn set_flags(&mut self, mflags: MappingFlags, _is_huge: bool) {
        let flags = PTEFlags::from(mflags) | PTEFlags::A | PTEFlags::D;
        debug_assert!(flags.intersects(PTEFlags::R | PTEFlags::X));
        self.0 = (self.0 & Self::PHYS_ADDR_MASK) | flags.bits() as u64;
        self.set_extended_flags(mflags, 0);
    }

    fn bits(self) -> usize {
        self.0 as usize
    }

    fn is_unused(&self) -> bool {
        self.0 == 0
    }

    fn is_present(&self) -> bool {
        PTEFlags::from_bits_truncate(self.0 as usize).contains(PTEFlags::V)
    }

    fn is_huge(&self) -> bool {
        PTEFlags::from_bits_truncate(self.0 as usize).intersects(PTEFlags::R | PTEFlags::X)
    }

    fn clear(&mut self) {
        self.0 = 0
    }
}

impl fmt::Debug for Rv64PTE {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut f = f.debug_struct("Rv64PTE");
        f.field("raw", &self.0)
            .field("paddr", &self.paddr())
            .field("flags", &self.flags())
            .finish()
    }
}
