mod paging {
    pub use ax_cpu::paging::MappingFlags;
}

// Compile target-owned PTE modules in the host test harness so their entry
// semantics can be exercised without a target-side test runner.
#[expect(
    dead_code,
    reason = "the host adapter exercises PTE behavior without architecture initialization"
)]
#[path = "../src/aarch64/paging.rs"]
mod aarch64_paging;
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

use aarch64_paging::A64Pte;
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
fn aarch64_relocated_normal_leaf_preserves_memory_type() {
    let source_paddr = PhysAddr::from_usize(0x1_81ea_5000);
    let target_paddr = PhysAddr::from_usize(0x1_8200_0000);
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let source_pte = A64Pte::new_page(source_paddr, flags, false);
    let queried_flags = source_pte.config(false);
    let target_pte = A64Pte::new_page(target_paddr, queried_flags, false);

    assert_eq!(queried_flags, flags);
    assert_eq!(target_pte.config(false), flags);
}

#[test]
fn aarch64_explicit_memory_types_roundtrip() {
    let paddr = PhysAddr::from_usize(0x1_81ea_5000);
    for memory_type in [MappingFlags::DEVICE, MappingFlags::UNCACHED] {
        let flags = MappingFlags::READ | MappingFlags::WRITE | memory_type;
        let pte = A64Pte::new_page(paddr, flags, false);

        assert_eq!(pte.config(false), flags);
    }
}

#[test]
fn aarch64_unprogrammed_mair_indices_decode_as_device() {
    let paddr = PhysAddr::from_usize(0x1_81ea_5000);
    let normal_flags = MappingFlags::READ | MappingFlags::WRITE;
    for index in 3..8 {
        let pte = A64Pte::new_page(paddr, normal_flags, false).with_attr_index(index);

        assert_eq!(
            pte.config(false),
            normal_flags | MappingFlags::DEVICE,
            "AttrIndx {index} must match its zero-valued MAIR slot"
        );
    }
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
