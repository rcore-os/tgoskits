extern crate alloc;

use alloc::{format, string::ToString};

use ranges_ext::RangeOp;

kernutil::define_type! {
    CoverageId(usize),
    CoverageSignedId(isize),
    CoverageAddr(usize, "{:#x}"),
}

#[test]
fn kernutil_memory_descriptor_rules_hold() {
    use kernutil::memory::{MemoryDescriptor, MemoryType, PageTableInfo};

    let descriptor = MemoryDescriptor::new_with_range(0x1000..0x1800, MemoryType::Ram);
    assert_eq!(descriptor.physical_start, 0x1000);
    assert_eq!(descriptor.size_in_bytes, 0x800);
    assert_eq!(descriptor.range(), 0x1000..0x1800);
    assert_eq!(descriptor.kind(), MemoryType::Ram);
    assert!(!descriptor.overwritable(&descriptor));
    assert!(format!("{descriptor:?}").contains("physical_start"));

    let aligned =
        MemoryDescriptor::new_with_range_aligned(0x1234..0x2345, MemoryType::Reserved, 0x1000);
    assert_eq!(aligned.physical_start, 0x1000);
    assert_eq!(aligned.size_in_bytes, 0x2000);
    assert_eq!(aligned.range(), 0x1000..0x3000);

    let aligned = MemoryDescriptor::new_aligned(0x1234, 0x100, MemoryType::KImage, 0x1000);
    assert_eq!(aligned.physical_start, 0x1000);
    assert_eq!(aligned.size_in_bytes, 0x1000);

    let free = MemoryDescriptor::new_with_range(0x4000..0x5000, MemoryType::Free);
    assert!(free.overwritable(&descriptor));
    let cloned = descriptor.clone_with_range(0x2000..0x2800);
    assert_eq!(cloned.physical_start, 0x2000);
    assert_eq!(cloned.size_in_bytes, 0x800);
    assert_eq!(cloned.memory_type, MemoryType::Ram);

    assert_eq!(MemoryType::Free.to_string(), "Free  ");
    assert_eq!(MemoryType::Ram.to_string(), "RAM   ");
    assert_eq!(MemoryType::KImage.to_string(), "KImg  ");
    assert_eq!(MemoryType::Reserved.to_string(), "Rsv   ");
    assert_eq!(MemoryType::Mmio.to_string(), "MMIO  ");
    assert_eq!(MemoryType::PerCpuData.to_string(), "PerCPU");
    assert_eq!(MemoryType::default(), MemoryType::Free);

    let page_table = PageTableInfo::zero();
    assert_eq!(page_table.asid, 0);
    assert_eq!(page_table.addr, 0);
    let page_table = PageTableInfo {
        asid: 7,
        addr: 0xdead_beef,
    };
    assert_eq!(page_table.asid, 7);
    assert_eq!(page_table.addr, 0xdead_beef);
}

#[test]
fn kernutil_memory_descriptor_boundary_rules_hold() {
    use kernutil::memory::{MemoryDescriptor, MemoryType};

    let zero = MemoryDescriptor::new_with_range(0x4000..0x4000, MemoryType::Mmio);
    assert_eq!(zero.size_in_bytes, 0);
    assert_eq!(zero.range(), 0x4000..0x4000);
    assert_eq!(zero.kind(), MemoryType::Mmio);
    assert!(!zero.overwritable(&zero));

    let exact =
        MemoryDescriptor::new_with_range_aligned(0x2000..0x3000, MemoryType::PerCpuData, 0x1000);
    assert_eq!(exact.physical_start, 0x2000);
    assert_eq!(exact.size_in_bytes, 0x1000);
    assert_eq!(exact.range(), 0x2000..0x3000);

    let reserved = MemoryDescriptor::new_aligned(0x2fff, 1, MemoryType::Reserved, 0x1000);
    assert_eq!(reserved.range(), 0x2000..0x3000);

    let cloned = zero.clone().clone_with_range(0x5000..0x5800);
    assert_eq!(cloned.physical_start, 0x5000);
    assert_eq!(cloned.size_in_bytes, 0x800);
    assert_eq!(cloned.memory_type, MemoryType::Mmio);
    assert_eq!(format!("{:?}", MemoryType::PerCpuData), "PerCpuData");
}

#[test]
fn kernutil_define_type_generated_rules_hold() {
    let mut id = CoverageId::new(0x1234);
    assert_eq!(id.raw(), 0x1234);
    assert_eq!(CoverageId::default().raw(), 0);
    assert_eq!(CoverageId::from(9).raw(), 9);
    assert_eq!(usize::from(CoverageId::new(11)), 11);

    assert_eq!(id.align_down(0x100).raw(), 0x1200);
    assert_eq!(id.align_up(0x1000).raw(), 0x2000);
    assert!(CoverageId::new(0x2000).is_aligned_to(0x1000));
    assert!(!CoverageId::new(0x2100).is_aligned_to(0x1000));

    id += 3;
    assert_eq!(id.raw(), 0x1237);
    id -= 7;
    assert_eq!(id.raw(), 0x1230);
    assert_eq!((id + 0x10).raw(), 0x1240);
    assert_eq!((id + CoverageId::new(0x20)).raw(), 0x1250);
    assert_eq!((id - 0x30).raw(), 0x1200);
    assert_eq!(id - CoverageId::new(0x1000), 0x230);

    assert!(CoverageId::new(1) < CoverageId::new(2));
    assert_eq!(CoverageId::new(1), CoverageId::new(1));
    assert_eq!(format!("{}", CoverageId::new(42)), "42");
    assert_eq!(format!("{:?}", CoverageId::new(42)), "CoverageId(42)");

    let signed = CoverageSignedId::new(-7);
    assert_eq!(signed.raw(), -7);
    assert_eq!(format!("{signed}"), "-7");

    let addr = CoverageAddr::new(0xfeed);
    assert_eq!(format!("{addr}"), "0xfeed");
    assert_eq!(format!("{addr:?}"), "CoverageAddr(0xfeed)");
}
