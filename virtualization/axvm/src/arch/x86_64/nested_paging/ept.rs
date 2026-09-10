//! VM mapping policy and geometry for CPU-owned Intel EPT descriptors.

use ax_cpu::paging::{EptEntry as Descriptor, EptFlags, EptMemoryType, PageTableEntry};
use axvm_types::{HostPhysAddr, MappingFlags};
use page_table_generic as ptg;

use super::runtime::flush_nested_page_table;

/// Translates VM policy into hardware flags without duplicating bit encoding.
fn descriptor_flags(flags: MappingFlags) -> EptFlags {
    let mut result = EptFlags::empty();
    result.set(EptFlags::READ, flags.contains(MappingFlags::READ));
    result.set(EptFlags::WRITE, flags.contains(MappingFlags::WRITE));
    result.set(EptFlags::EXECUTE, flags.contains(MappingFlags::EXECUTE));
    result.with_memory_type(
        if flags.intersects(MappingFlags::DEVICE | MappingFlags::UNCACHED) {
            EptMemoryType::Uncached
        } else {
            EptMemoryType::WriteBack
        },
    )
}

fn mapping_flags(flags: EptFlags) -> MappingFlags {
    let mut result = MappingFlags::empty();
    result.set(MappingFlags::READ, flags.contains(EptFlags::READ));
    result.set(MappingFlags::WRITE, flags.contains(EptFlags::WRITE));
    result.set(MappingFlags::EXECUTE, flags.contains(EptFlags::EXECUTE));
    result.set(
        MappingFlags::DEVICE,
        flags.memory_type() == Some(EptMemoryType::Uncached),
    );
    result
}

/// Binds the VM's mapping policy to the CPU-owned descriptor encoding.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub(super) struct EptEntry(Descriptor);

impl PageTableEntry for EptEntry {
    type PteConfig = MappingFlags;

    fn new_page(paddr: HostPhysAddr, config: MappingFlags, is_huge: bool) -> Self {
        // Preserve the VM owner's convention that an empty request discards
        // the backing address; the raw CPU descriptor imposes no such policy.
        if config.is_empty() {
            return Self(Descriptor::default());
        }
        Self(Descriptor::new_page(
            paddr,
            descriptor_flags(config),
            is_huge,
        ))
    }

    fn new_table(paddr: HostPhysAddr) -> Self {
        Self(Descriptor::new_table(paddr))
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
/// EPT geometry and invalidation callback supplied to the generic walker.
pub(super) struct EptPageTableMetadata;

impl ptg::TableMeta for EptPageTableMetadata {
    type P = EptEntry;

    const PAGE_SIZE: usize = ax_memory_addr::PAGE_SIZE_4K;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;
    const STRICT_ADDRESS_WIDTH: bool = true;

    fn flush(vaddr: Option<ptg::VirtAddr>) {
        flush_nested_page_table(vaddr);
    }
}
