//! AMD nested-paging policy using the CPU's long-mode descriptor encoding.

use ax_cpu::paging::{DescriptorFlags, NptEntry as Pte, PageTableEntry};
use axvm_types::{HostPhysAddr, MappingFlags};
use page_table_generic as ptg;

use super::runtime::flush_nested_page_table;

fn descriptor_flags(flags: MappingFlags) -> DescriptorFlags {
    let mut result = DescriptorFlags::PRESENT;
    result.set(
        DescriptorFlags::WRITABLE,
        flags.contains(MappingFlags::WRITE),
    );
    result.set(DescriptorFlags::USER, flags.contains(MappingFlags::USER));
    result.set(
        DescriptorFlags::NO_EXECUTE,
        !flags.contains(MappingFlags::EXECUTE),
    );
    if flags.intersects(MappingFlags::DEVICE | MappingFlags::UNCACHED) {
        result |= DescriptorFlags::NO_CACHE | DescriptorFlags::WRITE_THROUGH;
    }
    result
}

fn mapping_flags(flags: DescriptorFlags) -> MappingFlags {
    if !flags.contains(DescriptorFlags::PRESENT) {
        return MappingFlags::empty();
    }
    let mut result = MappingFlags::READ;
    result.set(
        MappingFlags::WRITE,
        flags.contains(DescriptorFlags::WRITABLE),
    );
    result.set(MappingFlags::USER, flags.contains(DescriptorFlags::USER));
    result.set(
        MappingFlags::EXECUTE,
        !flags.contains(DescriptorFlags::NO_EXECUTE),
    );
    result.set(
        MappingFlags::DEVICE,
        flags.contains(DescriptorFlags::NO_CACHE),
    );
    result
}

/// Binds VM mapping policy to the shared long-mode hardware descriptor.
/// NPT and native paging have the same encoding but distinct table owners.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub(super) struct NptEntry(Pte);

impl PageTableEntry for NptEntry {
    type PteConfig = MappingFlags;

    fn new_page(paddr: HostPhysAddr, config: MappingFlags, is_huge: bool) -> Self {
        if config.is_empty() {
            return Self(Pte::default());
        }
        let mut flags = descriptor_flags(config);
        flags.set(DescriptorFlags::HUGE_PAGE, is_huge);
        Self(Pte::from_parts(paddr, flags))
    }

    fn new_table(paddr: HostPhysAddr) -> Self {
        Self(Pte::new_table(paddr))
    }

    fn paddr(&self, is_dir: bool) -> HostPhysAddr {
        self.0.paddr(is_dir)
    }

    fn config(&self, _is_dir: bool) -> MappingFlags {
        mapping_flags(self.0.flags())
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

#[derive(Clone, Copy)]
/// NPT geometry and invalidation callback supplied to the generic walker.
pub(super) struct NptPageTableMetadata;

impl ptg::TableMeta for NptPageTableMetadata {
    type P = NptEntry;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(vaddr: Option<ptg::VirtAddr>) {
        flush_nested_page_table(vaddr);
    }
}
