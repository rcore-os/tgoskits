use range_alloc_arceos::RangeAllocator;
use std::ops::Range;

fn assert_fully_freed(allocator: &RangeAllocator<usize>, initial_range: Range<usize>) {
    assert_eq!(allocator.initial_range(), &initial_range);
    let len = initial_range.end - initial_range.start;
    let mut temp = allocator.clone();
    let res = temp.allocate_range(len);
    assert!(
        res.is_ok(),
        "Allocator should be fully merged and capable of allocating full size"
    );
    assert_eq!(res.unwrap(), initial_range);
}

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

#[test]
fn test_double_free_check() {
    let mut allocator = RangeAllocator::new(0..100);
    let r1 = allocator.allocate_range(10).unwrap();

    allocator.free_range(r1.clone());
}

#[test]
fn test_grow() {
    let mut allocator = RangeAllocator::new(0..100);
    let _ = allocator.allocate_range(100).unwrap();

    allocator.insert_range(100..200);

    let r2 = allocator
        .allocate_range(50)
        .expect("Should allow alloc after grow");
    assert_eq!(r2, 100..150);
}
