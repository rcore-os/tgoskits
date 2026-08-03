use alloc::{format, string::ToString};

use axtest::prelude::*;
use ranges_ext::RangeOp;

use crate as kernutil;

kernutil::define_type! {
    CoverageId(usize),
    CoverageSignedId(isize),
    CoverageAddr(usize, "{:#x}"),
}

#[axtest]
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

#[axtest]
fn kernutil_memory_descriptor_boundary_rules_hold() {
    use kernutil::memory::{MemoryDescriptor, MemoryType};

    let zero = MemoryDescriptor::new_with_range(0x4000..0x4000, MemoryType::Mmio);
    ax_assert_eq!(zero.size_in_bytes, 0);
    ax_assert_eq!(zero.range(), 0x4000..0x4000);
    ax_assert_eq!(zero.kind(), MemoryType::Mmio);
    ax_assert!(!zero.overwritable(&zero));

    let exact =
        MemoryDescriptor::new_with_range_aligned(0x2000..0x3000, MemoryType::PerCpuData, 0x1000);
    ax_assert_eq!(exact.physical_start, 0x2000);
    ax_assert_eq!(exact.size_in_bytes, 0x1000);
    ax_assert_eq!(exact.range(), 0x2000..0x3000);

    let reserved = MemoryDescriptor::new_aligned(0x2fff, 1, MemoryType::Reserved, 0x1000);
    ax_assert_eq!(reserved.range(), 0x2000..0x3000);

    let cloned = zero.clone().clone_with_range(0x5000..0x5800);
    ax_assert_eq!(cloned.physical_start, 0x5000);
    ax_assert_eq!(cloned.size_in_bytes, 0x800);
    ax_assert_eq!(cloned.memory_type, MemoryType::Mmio);
    ax_assert_eq!(format!("{:?}", MemoryType::PerCpuData), "PerCpuData");
}

#[axtest]
fn kernutil_define_type_generated_rules_hold() {
    let mut id = CoverageId::new(0x1234);
    ax_assert_eq!(id.raw(), 0x1234);
    ax_assert_eq!(CoverageId::default().raw(), 0);
    ax_assert_eq!(CoverageId::from(9).raw(), 9);
    ax_assert_eq!(usize::from(CoverageId::new(11)), 11);

    ax_assert_eq!(id.align_down(0x100).raw(), 0x1200);
    ax_assert_eq!(id.align_up(0x1000).raw(), 0x2000);
    ax_assert!(CoverageId::new(0x2000).is_aligned_to(0x1000));
    ax_assert!(!CoverageId::new(0x2100).is_aligned_to(0x1000));

    id += 3;
    ax_assert_eq!(id.raw(), 0x1237);
    id -= 7;
    ax_assert_eq!(id.raw(), 0x1230);
    ax_assert_eq!((id + 0x10).raw(), 0x1240);
    ax_assert_eq!((id + CoverageId::new(0x20)).raw(), 0x1250);
    ax_assert_eq!((id - 0x30).raw(), 0x1200);
    ax_assert_eq!(id - CoverageId::new(0x1000), 0x230);

    ax_assert!(CoverageId::new(1) < CoverageId::new(2));
    ax_assert_eq!(CoverageId::new(1), CoverageId::new(1));
    ax_assert_eq!(format!("{}", CoverageId::new(42)), "42");
    ax_assert_eq!(format!("{:?}", CoverageId::new(42)), "CoverageId(42)");

    let signed = CoverageSignedId::new(-7);
    ax_assert_eq!(signed.raw(), -7);
    ax_assert_eq!(format!("{signed}"), "-7");

    let addr = CoverageAddr::new(0xfeed);
    ax_assert_eq!(format!("{addr}"), "0xfeed");
    ax_assert_eq!(format!("{addr:?}"), "CoverageAddr(0xfeed)");
}
