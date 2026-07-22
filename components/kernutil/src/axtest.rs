use alloc::{format, string::ToString};

use axtest::prelude::*;
use ranges_ext::RangeOp;

use crate as kernutil;

#[axtest::def_test]
fn kernutil_memory_descriptor_rules_hold() {
    use kernutil::memory::{MemoryDescriptor, MemoryType, PageTableInfo};

    let descriptor = MemoryDescriptor::new_with_range(0x1000..0x1800, MemoryType::Ram);
    ax_assert_eq!(descriptor.physical_start, 0x1000);
    ax_assert_eq!(descriptor.size_in_bytes, 0x800);
    ax_assert_eq!(descriptor.range(), 0x1000..0x1800);
    ax_assert_eq!(descriptor.kind(), MemoryType::Ram);
    ax_assert!(!descriptor.overwritable(&descriptor));
    ax_assert!(format!("{descriptor:?}").contains("physical_start"));

    let aligned =
        MemoryDescriptor::new_with_range_aligned(0x1234..0x2345, MemoryType::Reserved, 0x1000);
    ax_assert_eq!(aligned.physical_start, 0x1000);
    ax_assert_eq!(aligned.size_in_bytes, 0x2000);
    ax_assert_eq!(aligned.range(), 0x1000..0x3000);

    let aligned = MemoryDescriptor::new_aligned(0x1234, 0x100, MemoryType::KImage, 0x1000);
    ax_assert_eq!(aligned.physical_start, 0x1000);
    ax_assert_eq!(aligned.size_in_bytes, 0x1000);

    let free = MemoryDescriptor::new_with_range(0x4000..0x5000, MemoryType::Free);
    ax_assert!(free.overwritable(&descriptor));
    let cloned = descriptor.clone_with_range(0x2000..0x2800);
    ax_assert_eq!(cloned.physical_start, 0x2000);
    ax_assert_eq!(cloned.size_in_bytes, 0x800);
    ax_assert_eq!(cloned.memory_type, MemoryType::Ram);

    ax_assert_eq!(MemoryType::Free.to_string(), "Free  ");
    ax_assert_eq!(MemoryType::Ram.to_string(), "RAM   ");
    ax_assert_eq!(MemoryType::KImage.to_string(), "KImg  ");
    ax_assert_eq!(MemoryType::Reserved.to_string(), "Rsv   ");
    ax_assert_eq!(MemoryType::Mmio.to_string(), "MMIO  ");
    ax_assert_eq!(MemoryType::PerCpuData.to_string(), "PerCPU");
    ax_assert_eq!(MemoryType::default(), MemoryType::Free);

    let page_table = PageTableInfo::zero();
    ax_assert_eq!(page_table.asid, 0);
    ax_assert_eq!(page_table.addr, 0);
    let page_table = PageTableInfo {
        asid: 7,
        addr: 0xdead_beef,
    };
    ax_assert_eq!(page_table.asid, 7);
    ax_assert_eq!(page_table.addr, 0xdead_beef);
}
