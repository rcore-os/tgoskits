//! AArch64 page-table descriptor format.

use ax_memory_addr::PhysAddr;
use page_table_generic::{MemAttributes, PageTableEntry, PteConfig};

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
        const NG = 1 << 11;
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

impl A64DescriptorAttr {
    const ATTR_INDEX_MASK: u64 = 0x1c;

    const fn from_mem_attr(idx: A64MemAttr) -> Self {
        let mut bits = (idx as u64) << 2;
        if matches!(idx, A64MemAttr::Normal | A64MemAttr::NormalNonCacheable) {
            bits |= Self::INNER.bits() | Self::SHAREABLE.bits();
        }
        Self::from_bits_retain(bits)
    }

    const fn mem_attr(self) -> Option<A64MemAttr> {
        let idx = (self.bits() & Self::ATTR_INDEX_MASK) >> 2;
        Some(match idx {
            0 => A64MemAttr::Device,
            1 => A64MemAttr::Normal,
            2 => A64MemAttr::NormalNonCacheable,
            _ => return None,
        })
    }
}

/// AArch64 VMSAv8-64 translation-table descriptor.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct A64Pte(u64);

impl A64Pte {
    const PHYS_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

    fn attr(self) -> A64DescriptorAttr {
        A64DescriptorAttr::from_bits_truncate(self.0)
    }

    fn leaf_attr(config: PteConfig) -> A64DescriptorAttr {
        let mem_attr = match config.mem_attr {
            MemAttributes::Device => A64MemAttr::Device,
            MemAttributes::Uncached => A64MemAttr::NormalNonCacheable,
            _ => A64MemAttr::Normal,
        };
        let mut attr = A64DescriptorAttr::from_mem_attr(mem_attr) | A64DescriptorAttr::AF;
        if config.read {
            attr |= A64DescriptorAttr::VALID;
        }
        if !config.writable {
            attr |= A64DescriptorAttr::AP_RO;
        }
        #[cfg(not(feature = "arm-el2"))]
        {
            if config.lower {
                attr |= A64DescriptorAttr::AP_EL0 | A64DescriptorAttr::PXN;
                if !config.executable {
                    attr |= A64DescriptorAttr::UXN;
                }
            } else {
                attr |= A64DescriptorAttr::UXN;
                if !config.executable {
                    attr |= A64DescriptorAttr::PXN;
                }
            }
        }
        #[cfg(feature = "arm-el2")]
        {
            if !config.executable {
                attr |= A64DescriptorAttr::UXN;
            }
        }
        attr
    }
}

impl PageTableEntry for A64Pte {
    fn from_config(config: PteConfig) -> Self {
        if !config.valid {
            return Self(0);
        }
        if config.is_dir && !config.huge {
            let attr = A64DescriptorAttr::NON_BLOCK | A64DescriptorAttr::VALID;
            return Self((config.paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | attr.bits());
        }

        let mut attr = Self::leaf_attr(config);
        if !config.huge {
            attr |= A64DescriptorAttr::NON_BLOCK;
        }
        Self((config.paddr.as_usize() as u64 & Self::PHYS_ADDR_MASK) | attr.bits())
    }

    fn to_config(&self, is_dir: bool) -> PteConfig {
        let attr = self.attr();
        let valid = attr.contains(A64DescriptorAttr::VALID);
        let huge = is_dir && !attr.contains(A64DescriptorAttr::NON_BLOCK);
        let mut config = PteConfig {
            paddr: PhysAddr::from_usize((self.0 & Self::PHYS_ADDR_MASK) as usize),
            valid,
            read: valid,
            writable: !attr.contains(A64DescriptorAttr::AP_RO),
            dirty: true,
            global: !attr.contains(A64DescriptorAttr::NG),
            is_dir: is_dir && valid && !huge,
            huge,
            mem_attr: match attr.mem_attr() {
                Some(A64MemAttr::Device) => MemAttributes::Device,
                Some(A64MemAttr::NormalNonCacheable) => MemAttributes::Uncached,
                _ => MemAttributes::Normal,
            },
            ..Default::default()
        };
        #[cfg(not(feature = "arm-el2"))]
        {
            config.lower = attr.contains(A64DescriptorAttr::AP_EL0);
            config.executable = if config.lower {
                !attr.contains(A64DescriptorAttr::UXN)
            } else {
                !attr.contains(A64DescriptorAttr::PXN)
            };
        }
        #[cfg(feature = "arm-el2")]
        {
            config.executable = !attr.contains(A64DescriptorAttr::UXN);
        }
        config
    }

    fn valid(&self) -> bool {
        self.attr().contains(A64DescriptorAttr::VALID)
    }
}

impl core::fmt::Debug for A64Pte {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("A64Pte")
            .field("raw", &self.0)
            .field("config", &self.to_config(false))
            .finish()
    }
}
