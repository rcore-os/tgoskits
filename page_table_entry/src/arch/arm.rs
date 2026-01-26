//! ARMv7-A Short-descriptor translation table format.
//!
//! This module implements page table entries for ARMv7-A architecture using
//! the Short-descriptor format, which supports 2-level page tables:
//! - L1 (Translation Table): 4096 entries, each mapping 1MB or pointing to L2
//! - L2 (Page Table): 256 entries, each mapping 4KB (Small Page)

use core::fmt;

use memory_addr::PhysAddr;

use crate::{GenericPTE, MappingFlags};

bitflags::bitflags! {
    /// ARMv7-A Short-descriptor page table entry flags.
    ///
    /// Reference: ARM Architecture Reference Manual ARMv7-A/R Edition
    /// Section B3.5: Short-descriptor translation table format
    #[derive(Debug, Clone, Copy)]
    pub struct DescriptorAttr: u32 {
        // Common bits for all descriptor types

        /// Bit[0]: Descriptor type bit 0
        /// Combined with bit[1] to determine descriptor type:
        /// - 00: Invalid/Fault
        /// - 01: Page Table (L1) or Large Page (L2)
        /// - 10: Section (L1) or Small Page (L2)
        /// - 11: Section (L1, PXN enabled) or Small Page (L2)
        const TYPE_BIT0 = 1 << 0;

        /// Bit[1]: Descriptor type bit 1
        const TYPE_BIT1 = 1 << 1;

        // Section/Page specific attributes (bits [2-17])

        /// Bit[2]: Bufferable (B) - Part of memory type encoding
        const B = 1 << 2;

        /// Bit[3]: Cacheable (C) - Part of memory type encoding
        const C = 1 << 3;

        /// Bit[4]: Execute Never (XN) for Small Pages
        const XN_SMALL = 1 << 4;

        /// Bits[5:4]: Domain for Sections (only applies to L1 Section entries)
        const DOMAIN_MASK = 0b1111 << 5;

        /// Bit[9]: Implementation defined
        const IMP = 1 << 9;

        /// Bits[11:10]: Access Permission bits [1:0]
        /// AP[2:0] encoding (with AP[2] from bit 15):
        /// - 000: No access
        /// - 001: Privileged RW, User no access
        /// - 010: Privileged RW, User RO
        /// - 011: Privileged RW, User RW
        /// - 101: Privileged RO, User no access
        /// - 110: Privileged RO, User RO (deprecated)
        /// - 111: Privileged RO, User RO
        const AP0 = 1 << 10;
        /// Bit[11]: Access Permission bit 1
        const AP1 = 1 << 11;

        /// Bits[14:12]: Type Extension (TEX) - Extended memory type
        const TEX0 = 1 << 12;
        /// Bit[13]: Type Extension bit 1
        const TEX1 = 1 << 13;
        /// Bit[14]: Type Extension bit 2
        const TEX2 = 1 << 14;

        /// Bit[15]: Access Permission bit [2]
        const AP2 = 1 << 15;

        /// Bit[16]: Shareable (S)
        const S = 1 << 16;

        /// Bit[17]: Not Global (nG)
        const NG = 1 << 17;

        /// Bit[18]: For Section: Not Secure (NS)
        const NS = 1 << 19;

        // Combined flags for common use cases

        /// Section descriptor type (1MB block)
        const SECTION = Self::TYPE_BIT1.bits();

        /// Page table descriptor type (points to L2)
        const PAGE_TABLE = Self::TYPE_BIT0.bits();

        /// Small page descriptor type (4KB page in L2)
        const SMALL_PAGE = Self::TYPE_BIT1.bits();

        /// Normal memory attributes (Inner/Outer Write-Back, Write-Allocate, Cacheable)
        /// TEX=001, C=1, B=1 -> Normal memory, Cacheable
        const NORMAL_MEMORY = Self::TEX0.bits() | Self::C.bits() | Self::B.bits();

        /// Device memory attributes (Device, nGnRnE)
        /// TEX=000, C=0, B=1 -> Device memory
        const DEVICE_MEMORY = Self::B.bits();

        /// Shareable attribute for normal memory
        const NORMAL_SHAREABLE = Self::NORMAL_MEMORY.bits() | Self::S.bits();

        /// Access permission: Privileged RW, User no access
        const AP_PRIV_RW = Self::AP0.bits();

        /// Access permission: Privileged RW, User RW
        const AP_USER_RW = Self::AP0.bits() | Self::AP1.bits();

        /// Access permission: Privileged RO, User no access
        const AP_PRIV_RO = Self::AP2.bits() | Self::AP0.bits();

        /// Access permission: Privileged RO, User RO
        const AP_USER_RO = Self::AP2.bits() | Self::AP0.bits() | Self::AP1.bits();
    }
}

impl DescriptorAttr {
    /// Returns the common flags for both Section (L1) and Small Page (L2)
    /// descriptors.
    #[inline]
    pub const fn common_flags(flags: MappingFlags) -> u32 {
        let mut bits = 0;

        // Memory type
        if flags.contains(MappingFlags::DEVICE) {
            bits |= Self::DEVICE_MEMORY.bits();
        } else if flags.contains(MappingFlags::UNCACHED) {
            // Uncached normal memory: TEX=001, C=0, B=0
            bits |= Self::TEX0.bits();
        } else {
            // Normal cacheable memory with shareable
            bits |= Self::NORMAL_SHAREABLE.bits();
        }

        // Access permissions
        let has_write = flags.contains(MappingFlags::WRITE);
        let has_user = flags.contains(MappingFlags::USER);

        if has_user {
            if has_write {
                bits |= Self::AP_USER_RW.bits();
            } else {
                bits |= Self::AP_USER_RO.bits();
            }
        } else if has_write {
            bits |= Self::AP_PRIV_RW.bits();
        } else {
            bits |= Self::AP_PRIV_RO.bits();
        }

        bits
    }

    /// Creates descriptor attributes from MappingFlags for a Section (1MB).
    #[inline]
    pub const fn from_mapping_flags_section(flags: MappingFlags) -> Self {
        let mut bits = Self::SECTION.bits();

        if flags.is_empty() {
            return Self::from_bits_retain(0);
        }

        bits |= Self::common_flags(flags);

        // Execute Never is in bit 4 for Sections when using LPAE
        // For standard ARMv7-A, XN is controlled via domain + TEX[0]
        // We use simplified model: if not executable, set appropriate bits
        if !flags.contains(MappingFlags::EXECUTE) {
            // XN for sections: can be indicated via domain or TEX settings
            // Here we assume XN support via bit 4 for supersection or AP settings
            bits |= Self::XN_SMALL.bits();
        }

        Self::from_bits_retain(bits)
    }

    /// Creates descriptor attributes from MappingFlags for a Small Page (4KB).
    #[inline]
    pub const fn from_mapping_flags_small_page(flags: MappingFlags) -> Self {
        let mut bits = Self::SMALL_PAGE.bits();

        if flags.is_empty() {
            return Self::from_bits_retain(0);
        }

        let attr = Self::common_flags(flags);

        // B(2) and C(3) are at the same positions for L1 and L2
        bits |= attr & (Self::B.bits() | Self::C.bits());

        // Other attributes are shifted right by 6 bits for L2 Small Page:
        // - AP[1:0]: 11:10 -> 5:4
        // - TEX[2:0]: 14:12 -> 8:6
        // - AP[2]: 15 -> 9
        // - S: 16 -> 10
        // - nG: 17 -> 11
        // Mask covers bits 10 to 17
        let shift_mask = Self::AP0.bits()
            | Self::AP1.bits()
            | Self::TEX0.bits()
            | Self::TEX1.bits()
            | Self::TEX2.bits()
            | Self::AP2.bits()
            | Self::S.bits()
            | Self::NG.bits();

        bits |= (attr & shift_mask) >> 6;

        // Execute Never for Small Pages (XN is bit 0 in L2)
        if !flags.contains(MappingFlags::EXECUTE) {
            bits |= Self::TYPE_BIT0.bits();
        }

        Self::from_bits_retain(bits)
    }

    /// Returns the descriptor type.
    pub const fn descriptor_type(&self) -> u32 {
        self.bits() & 0b11
    }

    /// Checks if this is a valid descriptor.
    pub const fn is_valid(&self) -> bool {
        self.descriptor_type() != 0
    }

    /// Checks if this is a Section descriptor (L1).
    pub const fn is_section(&self) -> bool {
        self.descriptor_type() == 0b10
    }

    /// Checks if this is a Page Table descriptor (L1).
    pub const fn is_page_table(&self) -> bool {
        self.descriptor_type() == 0b01
    }

    /// Checks if this is a Small Page descriptor (L2).
    pub const fn is_small_page(&self) -> bool {
        self.descriptor_type() == 0b10
    }
}

impl From<DescriptorAttr> for MappingFlags {
    #[inline]
    fn from(attr: DescriptorAttr) -> Self {
        if !attr.is_valid() {
            return Self::empty();
        }

        let mut flags = Self::READ;

        // Check write permission from AP bits
        let ap = ((attr.bits() >> 10) & 0b11) | (((attr.bits() >> 15) & 1) << 2);
        match ap {
            0b001 | 0b011 => flags |= Self::WRITE, // Privileged RW or User RW
            _ => {}
        }

        // Check user access
        if (ap & 0b10) != 0 {
            flags |= Self::USER;
        }

        // Check execute permission (XN bit)
        if !attr.contains(DescriptorAttr::XN_SMALL) {
            flags |= Self::EXECUTE;
        }

        // Check memory type
        let tex = (attr.bits() >> 12) & 0b111;
        let c = (attr.bits() >> 3) & 1;
        let b = (attr.bits() >> 2) & 1;

        if tex == 0 && c == 0 && b == 1 {
            flags |= Self::DEVICE;
        } else if tex == 1 && c == 0 && b == 0 {
            flags |= Self::UNCACHED;
        }

        flags
    }
}

/// An ARMv7-A Short-descriptor page table entry (32-bit).
///
/// This can represent:
/// - L1 Section descriptor (1MB mapping)
/// - L1 Page Table descriptor (points to L2 table)
/// - L2 Small Page descriptor (4KB mapping)
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct A32PTE(u32);

impl A32PTE {
    /// Physical address mask for Section (bits [31:20] for 1MB alignment)
    const SECTION_ADDR_MASK: u32 = 0xfff0_0000;

    /// Physical address mask for Page Table (bits [31:10] for 1KB alignment)
    const PAGE_TABLE_ADDR_MASK: u32 = 0xffff_fc00;

    /// Physical address mask for Small Page (bits [31:12] for 4KB alignment)
    const SMALL_PAGE_ADDR_MASK: u32 = 0xffff_f000;

    /// Creates an empty descriptor with all bits set to zero.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Creates a Section descriptor (1MB block).
    #[inline]
    pub const fn new_section(paddr: PhysAddr, flags: MappingFlags) -> Self {
        let attr = DescriptorAttr::from_mapping_flags_section(flags);
        Self(attr.bits() | (paddr.as_usize() as u32 & Self::SECTION_ADDR_MASK))
    }

    /// Creates a Small Page descriptor (4KB page).
    #[inline]
    pub const fn new_small_page(paddr: PhysAddr, flags: MappingFlags) -> Self {
        let attr = DescriptorAttr::from_mapping_flags_small_page(flags);
        Self(attr.bits() | (paddr.as_usize() as u32 & Self::SMALL_PAGE_ADDR_MASK))
    }

    /// Returns the descriptor type field.
    pub const fn descriptor_type(&self) -> u32 {
        self.0 & 0b11
    }

    /// Checks if this is a Section descriptor.
    pub const fn is_section(&self) -> bool {
        (self.0 & 0b11) == 0b10
    }
}

impl GenericPTE for A32PTE {
    #[inline]
    fn new_page(paddr: PhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
        if is_huge {
            // 1MB Section
            Self::new_section(paddr, flags)
        } else {
            // 4KB Small Page
            Self::new_small_page(paddr, flags)
        }
    }

    #[inline]
    fn new_table(paddr: PhysAddr) -> Self {
        // Page Table descriptor (L1 -> L2)
        let attr = DescriptorAttr::PAGE_TABLE;
        Self(attr.bits() | (paddr.as_usize() as u32 & Self::PAGE_TABLE_ADDR_MASK))
    }

    fn paddr(&self) -> PhysAddr {
        let desc_type = self.descriptor_type();
        let addr = match desc_type {
            0b01 => self.0 & Self::PAGE_TABLE_ADDR_MASK, // Page Table
            0b10 => {
                if (self.0 & Self::SECTION_ADDR_MASK) >= 0x10_0000 {
                    self.0 & Self::SECTION_ADDR_MASK // Section
                } else {
                    self.0 & Self::SMALL_PAGE_ADDR_MASK // Small Page
                }
            }
            0b11 => self.0 & Self::SMALL_PAGE_ADDR_MASK, // Small Page (XN=1)
            _ => 0,
        };
        PhysAddr::from(addr as usize)
    }

    fn flags(&self) -> MappingFlags {
        DescriptorAttr::from_bits_truncate(self.0).into()
    }

    fn set_paddr(&mut self, paddr: PhysAddr) {
        let desc_type = self.descriptor_type();
        match desc_type {
            0b01 => {
                // Page Table
                self.0 = (self.0 & !Self::PAGE_TABLE_ADDR_MASK)
                    | (paddr.as_usize() as u32 & Self::PAGE_TABLE_ADDR_MASK);
            }
            0b10 => {
                // Section or Small Page
                if self.is_section() {
                    self.0 = (self.0 & !Self::SECTION_ADDR_MASK)
                        | (paddr.as_usize() as u32 & Self::SECTION_ADDR_MASK);
                } else {
                    self.0 = (self.0 & !Self::SMALL_PAGE_ADDR_MASK)
                        | (paddr.as_usize() as u32 & Self::SMALL_PAGE_ADDR_MASK);
                }
            }
            _ => {}
        }
    }

    fn set_flags(&mut self, flags: MappingFlags, is_huge: bool) {
        let paddr = self.paddr();
        *self = if is_huge {
            Self::new_section(paddr, flags)
        } else {
            Self::new_small_page(paddr, flags)
        };
    }

    fn bits(self) -> usize {
        self.0 as usize
    }

    fn is_unused(&self) -> bool {
        self.0 == 0
    }

    fn is_present(&self) -> bool {
        self.descriptor_type() != 0
    }

    fn is_huge(&self) -> bool {
        self.is_section()
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

impl fmt::Debug for A32PTE {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut f = f.debug_struct("A32PTE");
        f.field("raw", &format_args!("{:#010x}", self.0))
            .field(
                "type",
                &match self.descriptor_type() {
                    0b00 => "Invalid",
                    0b01 => "PageTable",
                    0b10 => {
                        if self.is_section() {
                            "Section"
                        } else {
                            "SmallPage"
                        }
                    }
                    0b11 => "Reserved",
                    _ => "Unknown",
                },
            )
            .field("paddr", &self.paddr())
            .field("flags", &self.flags())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_section_descriptor() {
        let paddr = PhysAddr::from(0x4000_0000);
        let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE;
        let pte = A32PTE::new_section(paddr, flags);

        assert!(pte.is_present());
        assert!(pte.is_huge());
        assert_eq!(pte.paddr(), paddr);
        assert!(pte.flags().contains(MappingFlags::READ));
        assert!(pte.flags().contains(MappingFlags::WRITE));
    }

    #[test]
    fn test_small_page_descriptor() {
        let paddr = PhysAddr::from(0x4000_1000);
        let flags = MappingFlags::READ | MappingFlags::WRITE;
        let pte = A32PTE::new_small_page(paddr, flags);

        assert!(pte.is_present());
        assert_eq!(pte.paddr(), paddr);
        assert!(pte.flags().contains(MappingFlags::READ));
    }

    #[test]
    fn test_page_table_descriptor() {
        let paddr = PhysAddr::from(0x4000_0400);
        let pte = A32PTE::new_table(paddr);

        assert!(pte.is_present());
        assert!(!pte.is_huge());
        assert_eq!(pte.paddr(), paddr);
    }
}
