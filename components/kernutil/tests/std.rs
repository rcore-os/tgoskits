use ranges_ext::RangeOp;

#[test]
fn kernutil_memory_descriptor_rules_hold() {
    use kernutil::memory::{MemoryDescriptor, MemoryType};

    let descriptor = MemoryDescriptor::new_with_range(0x1000..0x1800, MemoryType::Ram);
    assert_eq!(descriptor.physical_start, 0x1000);
    assert_eq!(descriptor.size_in_bytes, 0x800);
    assert_eq!(descriptor.range(), 0x1000..0x1800);
    assert_eq!(descriptor.kind(), MemoryType::Ram);
    assert!(!descriptor.overwritable(&descriptor));

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
}
