extern crate alloc;

use alloc::{format, vec, vec::Vec};
use core::hash::{Hash, Hasher};

#[derive(Default)]
struct ByteHasher {
    value: u64,
}

impl Hasher for ByteHasher {
    fn finish(&self) -> u64 {
        self.value
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.value = self.value.wrapping_mul(131).wrapping_add(u64::from(*byte));
        }
    }
}

#[test]
fn cpumask_small_mask_bit_and_iteration_rules_hold() {
    use ax_cpumask::CpuMask;

    let empty = CpuMask::<8>::new();
    assert!(empty.is_empty());
    assert!(!empty.is_full());
    assert_eq!(empty.len(), 0);
    assert_eq!(empty.first_index(), None);
    assert_eq!(empty.last_index(), None);
    assert_eq!(empty.first_false_index(), Some(0));
    assert_eq!(empty.last_false_index(), Some(7));
    assert_eq!(empty.next_false_index(3), Some(4));
    assert_eq!(empty.prev_false_index(3), Some(7));
    assert_eq!(format!("{empty:?}"), "cpumask: []");

    let mut mask = CpuMask::<8>::new();
    assert!(!mask.set(1, true));
    assert!(mask.set(1, true));
    assert!(!mask.set(3, true));
    assert!(!mask.set(6, true));
    assert!(mask.get(1));
    assert!(mask.get(3));
    assert!(mask.get(6));
    assert_eq!(mask.len(), 3);
    assert_eq!(mask.first_index(), Some(1));
    assert_eq!(mask.next_index(1), Some(3));
    assert_eq!(mask.next_index(3), Some(6));
    assert_eq!(mask.next_index(6), None);
    assert_eq!(mask.last_index(), Some(6));
    assert_eq!(mask.prev_index(6), Some(3));
    assert_eq!(mask.prev_index(3), Some(1));
    assert_eq!(mask.prev_index(1), None);
    assert_eq!(mask.first_false_index(), Some(0));
    assert_eq!(mask.next_false_index(0), Some(2));
    assert_eq!(mask.prev_false_index(6), Some(7));
    assert_eq!(mask.last_false_index(), Some(7));

    let forward: Vec<_> = (&mask).into_iter().collect();
    assert_eq!(forward, vec![1, 3, 6]);
    let reverse: Vec<_> = (&mask).into_iter().rev().collect();
    assert_eq!(reverse, vec![6, 3, 1]);
    let mut iter = (&mask).into_iter();
    assert_eq!(iter.next(), Some(1));
    assert_eq!(iter.next_back(), Some(6));
    assert_eq!(iter.next(), Some(3));
    assert_eq!(iter.next_back(), Some(3));
    assert_eq!(iter.next_back(), None);
    assert_eq!(iter.next(), None);

    let mut inverted = mask;
    inverted.invert();
    assert!(!inverted.get(1));
    assert!(!inverted.get(3));
    assert!(!inverted.get(6));
    assert!(inverted.get(0));
    assert_eq!(inverted.len(), 5);
    assert_eq!(
        (&inverted).into_iter().collect::<Vec<_>>(),
        vec![0, 2, 4, 5, 7]
    );

    let full = CpuMask::<8>::full();
    assert!(full.is_full());
    assert_eq!(full.len(), 8);
    assert_eq!(full.first_false_index(), None);
    assert_eq!(full.last_false_index(), None);
    assert_eq!(full.next_false_index(0), None);
    assert_eq!(full.prev_false_index(7), None);
}

#[test]
fn cpumask_bit_ops_value_hash_and_order_rules_hold() {
    use ax_cpumask::CpuMask;

    let left = CpuMask::<8>::from_raw_bits(0b1010_1100);
    let right = CpuMask::<8>::from_raw_bits(0b0110_0110);
    assert_eq!((left & right).into_value(), 0b0010_0100);
    assert_eq!((left | right).into_value(), 0b1110_1110);
    assert_eq!((left ^ right).into_value(), 0b1100_1010);
    assert_eq!(
        (!CpuMask::<8>::from_raw_bits(0b0000_1111)).into_value(),
        0b1111_0000
    );

    let mut assigned = left;
    assigned &= right;
    assert_eq!(assigned.into_value(), 0b0010_0100);
    assigned |= CpuMask::<8>::from_raw_bits(0b0000_0011);
    assert_eq!(assigned.into_value(), 0b0010_0111);
    assigned ^= CpuMask::<8>::from_raw_bits(0b0010_0001);
    assert_eq!(assigned.into_value(), 0b0000_0110);

    let prefix = CpuMask::<8>::mask(5);
    assert_eq!(prefix.into_value(), 0b0001_1111);
    assert_eq!(CpuMask::<8>::one_shot(4).into_value(), 0b0001_0000);
    assert_eq!(
        CpuMask::<8>::from_value(0b0101_0101).into_value(),
        0b0101_0101
    );
    assert_eq!(*CpuMask::<8>::from_raw_bits(0b11).as_value(), 0b11);
    assert_eq!(CpuMask::<8>::from_raw_bits(0b11).as_bytes()[0] & 0b11, 0b11);

    let low = CpuMask::<8>::from_raw_bits(0b0000_0001);
    let high = CpuMask::<8>::from_raw_bits(0b1000_0000);
    assert!(low < high);
    assert_eq!(low.partial_cmp(&high), Some(core::cmp::Ordering::Less));

    let mut low_hasher = ByteHasher::default();
    low.hash(&mut low_hasher);
    let mut high_hasher = ByteHasher::default();
    high.hash(&mut high_hasher);
    assert_ne!(low_hasher.finish(), high_hasher.finish());

    let large = CpuMask::<256>::from([1_u128 << 127, 1_u128 << 5]);
    assert!(large.get(127));
    assert!(large.get(128 + 5));
    assert_eq!(large.len(), 2);
    let raw: [u128; 2] = large.into();
    assert_eq!(raw, [1_u128 << 127, 1_u128 << 5]);

    let larger = CpuMask::<512>::from([0_u128, 0, 1_u128 << 63, 0]);
    assert!(larger.get(256 + 63));
    let raw: [u128; 4] = larger.into();
    assert_eq!(raw[2], 1_u128 << 63);
}

#[test]
fn cpumask_large_array_conversion_rules_hold() {
    use ax_cpumask::CpuMask;

    let cases_384 = [1_u128, 1_u128 << 64, 1_u128 << 127];
    let mask = CpuMask::<384>::from(cases_384);
    assert!(mask.get(0));
    assert!(mask.get(128 + 64));
    assert!(mask.get(256 + 127));
    assert_eq!(mask.len(), 3);
    let raw: [u128; 3] = mask.into();
    assert_eq!(raw, cases_384);

    let cases_640 = [0_u128, 2, 4, 8, 16];
    let mask = CpuMask::<640>::from(cases_640);
    assert!(mask.get(128 + 1));
    assert!(mask.get(256 + 2));
    assert!(mask.get(384 + 3));
    assert!(mask.get(512 + 4));
    let raw: [u128; 5] = mask.into();
    assert_eq!(raw, cases_640);

    let cases_768 = [1_u128, 0, 0, 0, 0, 1_u128 << 9];
    let mask = CpuMask::<768>::from(cases_768);
    assert_eq!(mask.first_index(), Some(0));
    assert_eq!(mask.last_index(), Some(640 + 9));
    let raw: [u128; 6] = mask.into();
    assert_eq!(raw, cases_768);

    let cases_896 = [0_u128, 0, 0, 0, 0, 0, 1_u128 << 17];
    let mask = CpuMask::<896>::from(cases_896);
    assert_eq!(mask.first_index(), Some(768 + 17));
    let raw: [u128; 7] = mask.into();
    assert_eq!(raw, cases_896);

    let cases_1024 = [0_u128, 0, 0, 0, 0, 0, 0, 1_u128 << 31];
    let mask = CpuMask::<1024>::from(cases_1024);
    assert_eq!(mask.last_index(), Some(896 + 31));
    let raw: [u128; 8] = mask.into();
    assert_eq!(raw, cases_1024);
}

#[test]
fn cpumask_iterator_clone_debug_and_empty_crossing_rules_hold() {
    use ax_cpumask::CpuMask;

    let mask = CpuMask::<16>::from_raw_bits(0b1000_0000_0000_0001);
    let mut iter = (&mask).into_iter();

    // Test basic iteration
    assert_eq!(iter.next(), Some(0));
    assert_eq!(iter.next(), Some(15));
    assert_eq!(iter.next(), None);

    // Test clone
    let mask2 = CpuMask::<16>::from_raw_bits(0b1000_0000_0000_0001);
    let iter2 = (&mask2).into_iter();
    let _cloned = iter2.clone();

    // Test empty mask
    let empty = CpuMask::<16>::new();
    let mut iter3 = (&empty).into_iter();
    assert_eq!(iter3.next(), None);
}
