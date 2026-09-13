//! Boot mapping policy over the CPU-owned stage-one descriptor.

#[cfg(not(feature = "hv"))]
use ax_cpu::paging::El1Pte as Pte;
#[cfg(feature = "hv")]
use ax_cpu::paging::El2Pte as Pte;
use ax_cpu::{
    PhysAddr,
    paging::{DescriptorFlags, PageTableEntry, TableMeta},
};

use crate::mem::{MemAttributes, PteConfig};

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Entry(Pte);

impl Entry {
    /// Returns an unused descriptor.
    pub const fn empty() -> Self {
        Self(Pte::from_parts(
            PhysAddr::from_usize(0),
            DescriptorFlags::empty(),
        ))
    }
}

impl PageTableEntry for Entry {
    type PteConfig = PteConfig;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, is_huge: bool) -> Self {
        let mut flags = DescriptorFlags::VALID;
        flags.set(DescriptorFlags::AF, config.read || config.dirty);
        flags.set(DescriptorFlags::NON_BLOCK, !is_huge);
        flags.set(DescriptorFlags::AP_RO, !config.writable);
        flags.set(DescriptorFlags::NON_GLOBAL, !config.global);
        #[cfg(not(feature = "hv"))]
        {
            if config.lower {
                flags |= DescriptorFlags::AP_EL0 | DescriptorFlags::PXN;
                flags.set(DescriptorFlags::UXN, !config.executable);
            } else {
                flags |= DescriptorFlags::UXN;
                flags.set(DescriptorFlags::PXN, !config.executable);
            }
        }
        #[cfg(feature = "hv")]
        flags.set(DescriptorFlags::UXN, !config.executable);

        let (slot, shareability) = match config.mem_attr {
            MemAttributes::Device => (0, DescriptorFlags::SH_OUTER),
            // CPU-local aliases remain ordinary coherent RAM, including when
            // another CPU accesses their published runtime state.
            MemAttributes::Normal | MemAttributes::PerCpu => (1, DescriptorFlags::SH_INNER),
            MemAttributes::Uncached => (2, DescriptorFlags::SH_OUTER),
        };
        flags |= shareability;
        flags = flags
            .with_attribute_index(slot)
            .expect("boot MAIR slots are 0..3");
        Self(Pte::from_parts(paddr, flags))
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self(Pte::from_parts(
            paddr,
            DescriptorFlags::VALID | DescriptorFlags::NON_BLOCK,
        ))
    }

    fn paddr(&self, is_dir: bool) -> PhysAddr {
        self.0.paddr(is_dir)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let flags = self.0.flags();
        let lower = flags.contains(DescriptorFlags::AP_EL0);
        #[cfg(not(feature = "hv"))]
        let executable = !flags.contains(if lower {
            DescriptorFlags::UXN
        } else {
            DescriptorFlags::PXN
        });
        #[cfg(feature = "hv")]
        let executable = !flags.contains(DescriptorFlags::UXN);
        PteConfig {
            read: flags.contains(DescriptorFlags::AF),
            writable: !flags.contains(DescriptorFlags::AP_RO),
            executable,
            lower,
            dirty: flags.contains(DescriptorFlags::AF),
            global: !flags.contains(DescriptorFlags::NON_GLOBAL),
            mem_attr: match flags.attribute_index() {
                0 => MemAttributes::Device,
                2 => MemAttributes::Uncached,
                _ => MemAttributes::Normal,
            },
        }
    }

    fn present(&self) -> bool {
        self.0.present()
    }
    fn huge(&self, is_dir: bool) -> bool {
        self.0.huge(is_dir)
    }
    fn unused(&self) -> bool {
        self.0.unused()
    }
    fn clear(&mut self) {
        self.0.clear();
    }
}

impl core::fmt::Debug for Entry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PTE {:?}", PageTableEntry::paddr(self, false))
    }
}

#[cfg(page_size_4k)]
#[derive(Clone, Copy)]
pub struct Generic;

impl TableMeta for Generic {
    type P = Entry;

    const PAGE_SIZE: usize = 0x1000;

    const LEVEL_BITS: &'static [usize] = &[9, 9, 9, 9];

    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(vaddr: Option<page_table_generic::VirtAddr>) {
        super::super::elx::flush_tlb(vaddr);
    }
}
