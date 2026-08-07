mod paging {
    pub use ax_cpu::paging::MappingFlags;
}

#[path = "../src/riscv/paging.rs"]
mod riscv_paging;

use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use page_table_generic::PageTableEntry;
use paging::MappingFlags;
use riscv_paging::Rv64Pte;

#[test]
fn non_present_huge_leaf_retains_its_structure() {
    assert_eq!(riscv_paging::PAGE_SIZE, PAGE_SIZE_4K);
    assert_eq!(riscv_paging::LEVEL_BITS, &[9, 9, 9]);
    assert_eq!(riscv_paging::MAX_BLOCK_LEVEL, 3);

    let paddr = PhysAddr::from_usize(0x4000_0000);
    let pte = Rv64Pte::new_page(paddr, MappingFlags::empty(), true);

    assert!(!pte.present());
    assert!(!pte.unused());
    assert!(pte.huge(true));
    assert_eq!(pte.paddr(false), paddr);
}
