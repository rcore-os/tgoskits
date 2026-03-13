use range_alloc_arceos::RangeAllocator;

#[test]
fn test_simple_allocation() {
    let mut allocator = RangeAllocator::new(0..100);

    let r1 = allocator.allocate_range(10).expect("Alloc 10 failed");
    assert_eq!(r1, 0..10);

    let r2 = allocator.allocate_range(20).expect("Alloc 20 failed");
    assert_eq!(r2, 10..30);

    allocator.free_range(r1);

    let r3 = allocator.allocate_range(5).expect("Alloc 5 failed");
    assert_eq!(r3, 0..5);
}

#[test]
fn test_out_of_memory() {
    let mut allocator = RangeAllocator::new(0..10);

    let _r1 = allocator.allocate_range(10).unwrap();

    let r2 = allocator.allocate_range(1);
    assert!(r2.is_err(), "Should return error when OOM");
}

#[test]
fn test_fragmentation_and_merge() {
    let mut allocator = RangeAllocator::new(0..100);

    let a = allocator.allocate_range(20).unwrap();
    let b = allocator.allocate_range(20).unwrap();
    let c = allocator.allocate_range(20).unwrap();
    let _d = allocator.allocate_range(40).unwrap();

    allocator.free_range(a);
    allocator.free_range(c);

    assert!(allocator.allocate_range(30).is_err());

    allocator.free_range(b);

    let big = allocator
        .allocate_range(60)
        .expect("Should merge ranges A, B, C");
    assert_eq!(big, 0..60);
}

#[test]
fn test_alignment_gaps() {
    let mut allocator = RangeAllocator::new(1000..2000);

    let r1 = allocator.allocate_range(100).unwrap();
    assert_eq!(r1, 1000..1100);

    let r2 = allocator.allocate_range(100).unwrap();
    assert_eq!(r2, 1100..1200);
}
