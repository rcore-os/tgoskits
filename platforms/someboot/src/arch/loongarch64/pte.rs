//! Boot mapping policy over the CPU-owned LoongArch descriptor.

use ax_cpu::{
    PhysAddr,
    paging::{DescriptorFlags, PageTableEntry, Pte},
};

use crate::mem::{MemAttributes, PteConfig};

#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
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
        let mut flags = DescriptorFlags::V | DescriptorFlags::P;
        flags.set(DescriptorFlags::NR, !config.read);
        flags.set(DescriptorFlags::W | DescriptorFlags::D, config.writable);
        flags.set(DescriptorFlags::NX, !config.executable);
        flags.set(DescriptorFlags::PLVL | DescriptorFlags::PLVH, config.lower);
        flags.set(DescriptorFlags::GH, is_huge || config.global);
        flags.set(DescriptorFlags::G, is_huge && config.global);
        flags |= match config.mem_attr {
            MemAttributes::Device => DescriptorFlags::empty(),
            MemAttributes::Normal | MemAttributes::PerCpu => DescriptorFlags::MATL,
            MemAttributes::Uncached => DescriptorFlags::MATH,
        };
        Self(Pte::from_parts(paddr, flags))
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self(Pte::new_table(paddr))
    }

    fn paddr(&self, is_dir: bool) -> PhysAddr {
        PageTableEntry::paddr(&self.0, is_dir)
    }

    fn config(&self, is_dir: bool) -> Self::PteConfig {
        let flags = self.0.flags();
        let global = if self.huge(is_dir) {
            flags.contains(DescriptorFlags::G)
        } else {
            flags.contains(DescriptorFlags::GH)
        };
        let memory_type = flags & (DescriptorFlags::MATL | DescriptorFlags::MATH);
        let mem_attr = if memory_type.is_empty() {
            MemAttributes::Device
        } else if memory_type.bits() == DescriptorFlags::MATH.bits() {
            MemAttributes::Uncached
        } else {
            MemAttributes::Normal
        };
        PteConfig {
            read: self.present() && !flags.contains(DescriptorFlags::NR),
            writable: flags.contains(DescriptorFlags::W),
            executable: !flags.contains(DescriptorFlags::NX),
            lower: flags.contains(DescriptorFlags::PLVL | DescriptorFlags::PLVH),
            dirty: flags.contains(DescriptorFlags::D),
            global,
            mem_attr,
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
