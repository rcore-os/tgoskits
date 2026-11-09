//! EPT descriptor integration with the production walker and frame allocator.

use ax_cpu::{
    PhysAddr, VirtAddr,
    paging::{EptEntry, EptFlags, EptMemoryType, PageTableEntry, TableMeta},
};
use ax_hal::paging::{MapConfig, PagingAllocator};

#[derive(Clone, Copy)]
struct DetachedEpt;

impl TableMeta for DetachedEpt {
    type P = EptEntry;

    const PAGE_SIZE: usize = 4096;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;
    const STRICT_ADDRESS_WIDTH: bool = true;

    // These allocated tables are never installed in an EPTP. No processor
    // can cache a translation from them, so they require no invalidation.
    fn flush(_address: Option<VirtAddr>) {}
}

pub(super) fn run() {
    use someboot::PageTable as GenericPageTable;

    // Use the runtime allocator while selecting a distinct CPU descriptor;
    // the ordinary HAL table alias intentionally remains native stage one.
    let mut table = GenericPageTable::<DetachedEpt, PagingAllocator>::new(PagingAllocator).unwrap();
    let base = VirtAddr::from_usize(0x4000_0000);
    let physical = PhysAddr::from_usize(0x8000_0000);
    for memory_type in [
        EptMemoryType::Uncached,
        EptMemoryType::WriteCombining,
        EptMemoryType::WriteThrough,
        EptMemoryType::WriteProtected,
        EptMemoryType::WriteBack,
    ] {
        let flags = (EptFlags::READ | EptFlags::WRITE).with_memory_type(memory_type);
        table
            .map(&MapConfig {
                vaddr: base,
                paddr: physical,
                size: 4096,
                pte: flags,
                allow_huge: false,
                flush: false,
            })
            .unwrap();
        let (observed, attributes, size) = table.query(base + 17).unwrap();
        assert_eq!(observed, physical + 17);
        assert_eq!(attributes, flags);
        assert_eq!(attributes.memory_type(), Some(memory_type));
        assert_eq!(size, 4096);
        table.unmap(base, 4096).unwrap();
        assert!(table.query(base).is_err());
    }
    let flags = (EptFlags::READ | EptFlags::EXECUTE).with_memory_type(EptMemoryType::WriteBack);
    table
        .map(&MapConfig {
            vaddr: base,
            paddr: physical,
            size: 2 * 1024 * 1024,
            pte: flags,
            allow_huge: true,
            flush: false,
        })
        .unwrap();
    let (observed, attributes, size) = table.query(base + 0x12345).unwrap();
    assert_eq!(observed, physical + 0x12345);
    assert_eq!(size, 2 * 1024 * 1024);
    assert_eq!(attributes, flags | EptFlags::HUGE_PAGE);
    table.unmap(base, size).unwrap();

    for reserved in [2, 3, 7] {
        assert_eq!(
            EptFlags::from_bits_retain(reserved << 3).memory_type(),
            None
        );
    }
    let mut retained = EptEntry::from_parts(physical, EptFlags::HUGE_PAGE);
    assert!(!retained.present());
    assert!(retained.huge(true));
    assert!(!retained.unused());
    assert_eq!(retained.paddr(false), physical);
    retained.clear();
    assert!(retained.unused());
}
