//! Boot mapping policy over CPU-owned RISC-V descriptors.

use ax_cpu::{
    PhysAddr, VirtAddr,
    paging::{DescriptorFlags, PageTableEntry, Pte, TableMeta},
};

use crate::mem::{MemAttributes, PteConfig};

#[derive(Clone, Copy, Debug, Default)]
#[repr(transparent)]
pub struct Entry(Pte);

impl PageTableEntry for Entry {
    type PteConfig = PteConfig;

    fn new_page(paddr: PhysAddr, config: Self::PteConfig, _is_huge: bool) -> Self {
        let mut flags = DescriptorFlags::V | DescriptorFlags::A;
        flags.set(DescriptorFlags::R, config.read);
        flags.set(DescriptorFlags::W, config.writable);
        flags.set(DescriptorFlags::X, config.executable);
        flags.set(DescriptorFlags::U, config.lower);
        flags.set(DescriptorFlags::G, config.global);
        flags.set(DescriptorFlags::D, config.writable || config.dirty);
        #[cfg(feature = "thead-mae")]
        {
            flags |= DescriptorFlags::SH;
            match config.mem_attr {
                MemAttributes::Device => flags |= DescriptorFlags::SO,
                MemAttributes::Uncached => flags |= DescriptorFlags::B,
                MemAttributes::Normal | MemAttributes::PerCpu => {
                    flags |= DescriptorFlags::B | DescriptorFlags::C;
                }
            }
        }
        Self(Pte::from_parts(paddr, flags))
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self(Pte::from_parts(paddr, DescriptorFlags::V))
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        self.0.paddr(false)
    }

    fn config(&self, _is_dir: bool) -> Self::PteConfig {
        let flags = self.0.flags();
        PteConfig {
            read: flags.contains(DescriptorFlags::R),
            writable: flags.contains(DescriptorFlags::W),
            executable: flags.contains(DescriptorFlags::X),
            lower: flags.contains(DescriptorFlags::U),
            global: flags.contains(DescriptorFlags::G),
            dirty: flags.contains(DescriptorFlags::D),
            mem_attr: memory_attribute(flags),
        }
    }

    fn present(&self) -> bool {
        self.0.present()
    }

    fn huge(&self, is_dir: bool) -> bool {
        // Boot mappings do not use the runtime's software non-present marker.
        is_dir
            && self
                .0
                .flags()
                .intersects(DescriptorFlags::R | DescriptorFlags::W | DescriptorFlags::X)
    }

    fn unused(&self) -> bool {
        self.0.unused()
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

#[cfg(feature = "thead-mae")]
fn memory_attribute(flags: DescriptorFlags) -> MemAttributes {
    let attributes = flags
        & (DescriptorFlags::SEC
            | DescriptorFlags::SH
            | DescriptorFlags::B
            | DescriptorFlags::C
            | DescriptorFlags::SO);
    if attributes.bits() == (DescriptorFlags::SO | DescriptorFlags::SH).bits() {
        MemAttributes::Device
    } else if attributes.bits() == (DescriptorFlags::B | DescriptorFlags::SH).bits() {
        MemAttributes::Uncached
    } else {
        MemAttributes::Normal
    }
}

#[cfg(not(feature = "thead-mae"))]
fn memory_attribute(_flags: DescriptorFlags) -> MemAttributes {
    MemAttributes::Normal
}

#[derive(Clone, Copy)]
pub struct Generic;

impl TableMeta for Generic {
    type P = Entry;

    const PAGE_SIZE: usize = 0x1000;
    const LEVEL_BITS: &'static [usize] = &[9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 1;

    fn flush(_vaddr: Option<VirtAddr>) {
        ax_cpu::mmu::flush_tlb(None);
    }
}
