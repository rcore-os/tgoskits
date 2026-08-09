mod paging {
    pub use ax_cpu::paging::MappingFlags;
}

// Compile target-owned PTE modules in the host test harness so their entry
// semantics can be exercised without a target-side test runner.
#[expect(
    dead_code,
    reason = "the host adapter exercises PTE behavior without architecture initialization"
)]
#[path = "../src/loongarch64/paging.rs"]
mod loongarch64_paging;
#[expect(
    dead_code,
    reason = "the host adapter exercises PTE behavior without architecture initialization"
)]
#[path = "../src/riscv/paging.rs"]
mod riscv_paging;

use ax_cpu::trap::PageFaultFlags;
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr};
use loongarch64_paging::La64Pte;
use page_table_generic::{PageTableEntry, TableMeta};
use paging::MappingFlags;
use riscv_paging::Rv64Pte;

#[test]
fn paging_metadata_is_available_without_feature_gates() {
    assert_eq!(ax_cpu::paging::ArchPagingMeta::PAGE_SIZE, PAGE_SIZE_4K);
}

#[test]
fn page_fault_access_converts_to_mapping_permissions() {
    let access = PageFaultFlags::READ | PageFaultFlags::WRITE | PageFaultFlags::USER;
    assert_eq!(
        MappingFlags::from(access),
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER
    );
}

#[test]
fn non_present_riscv_huge_leaf_retains_its_structure() {
    let paddr = PhysAddr::from_usize(0x4000_0000);
    let pte = Rv64Pte::new_page(paddr, MappingFlags::empty(), true);

    assert!(!pte.present());
    assert!(!pte.unused());
    assert!(pte.huge(true));
    assert_eq!(pte.paddr(false), paddr);
}

#[test]
fn non_present_loongarch_base_leaf_is_not_a_table() {
    let paddr = PhysAddr::from_usize(0x2345_6000);
    let pte = La64Pte::new_page(paddr, MappingFlags::empty(), false);

    assert!(!pte.present());
    assert!(!pte.unused());
    assert_eq!(pte.paddr(false), paddr);
}
