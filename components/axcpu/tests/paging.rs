use ax_memory_addr::PAGE_SIZE_4K;
use page_table_generic::TableMeta;

#[test]
fn paging_metadata_is_available_without_feature_gates() {
    assert_eq!(ax_cpu::paging::ArchPagingMeta::PAGE_SIZE, PAGE_SIZE_4K);
}

#[test]
fn page_fault_access_converts_to_mapping_permissions() {
    use ax_cpu::{paging::MappingFlags, trap::PageFaultFlags};

    let access = PageFaultFlags::READ | PageFaultFlags::WRITE | PageFaultFlags::USER;
    assert_eq!(
        MappingFlags::from(access),
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER
    );
}
