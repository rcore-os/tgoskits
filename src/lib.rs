#![no_std]
#![feature(asm)]

use bit_field::BitField;
use core::ops::Range;

/// Allocator of a bitmap, able to allocate / free bits.
pub trait BitAlloc: Default {
    /// The bitmap has a total of CAP bits, numbered from 0 to CAP-1 inclusively.
    const CAP: usize;

    /// The default value. Workaround for `const fn new() -> Self`.
    const DEFAULT: Self;

    /// Allocate a free bit.
    fn alloc(&mut self) -> Option<usize>;

    /// Allocate a free block with a given size, and return the first bit position.
    fn alloc_contiguous(&mut self, size: usize, align_log2: usize) -> Option<usize>;

    /// Find a index not less than a given key, where the bit is free.
    fn next(&self, key: usize) -> Option<usize>;

    /// Free an allocated bit.
    fn dealloc(&mut self, key: usize);

    /// Mark bits in the range as unallocated (available)
    fn insert(&mut self, range: Range<usize>);

    /// Reverse of insert
    fn remove(&mut self, range: Range<usize>);

    /// Whether there are free bits remaining
    fn any(&self) -> bool;

    /// Whether a specific bit is free
    fn test(&self, key: usize) -> bool;
}

/// A bitmap of 256 bits
pub type BitAlloc256 = BitAllocCascade16<BitAlloc16>;
/// A bitmap of 4K bits
pub type BitAlloc4K = BitAllocCascade16<BitAlloc256>;
/// A bitmap of 64K bits
pub type BitAlloc64K = BitAllocCascade16<BitAlloc4K>;
/// A bitmap of 1M bits
pub type BitAlloc1M = BitAllocCascade16<BitAlloc64K>;
/// A bitmap of 16M bits
pub type BitAlloc16M = BitAllocCascade16<BitAlloc1M>;
/// A bitmap of 256M bits
pub type BitAlloc256M = BitAllocCascade16<BitAlloc16M>;

/// Implement the bit allocator by segment tree algorithm.
#[derive(Default)]
pub struct BitAllocCascade16<T: BitAlloc> {
    bitset: u16, // for each bit, 1 indicates available, 0 indicates inavailable
    sub: [T; 16],
}

impl<T: BitAlloc> BitAlloc for BitAllocCascade16<T> {
    const CAP: usize = T::CAP * 16;

    const DEFAULT: Self = BitAllocCascade16 {
        bitset: 0,
        sub: [T::DEFAULT; 16],
    };

    fn alloc(&mut self) -> Option<usize> {
        if self.any() {
            let i = log2(self.bitset);
            let res = self.sub[i].alloc().unwrap() + i * T::CAP;
            self.bitset.set_bit(i, self.sub[i].any());
            Some(res)
        } else {
            None
        }
    }
    fn alloc_contiguous(&mut self, size: usize, align_log2: usize) -> Option<usize> {
        if let Some(base) = find_contiguous(self, Self::CAP, size, align_log2) {
            self.remove(base..base + size);
            Some(base)
        } else {
            None
        }
    }
    fn dealloc(&mut self, key: usize) {
        let i = key / T::CAP;
        self.sub[i].dealloc(key % T::CAP);
        self.bitset.set_bit(i, true);
    }
    fn insert(&mut self, range: Range<usize>) {
        self.for_range(range, |sub: &mut T, range| sub.insert(range));
    }
    fn remove(&mut self, range: Range<usize>) {
        self.for_range(range, |sub: &mut T, range| sub.remove(range));
    }
    fn any(&self) -> bool {
        self.bitset != 0
    }
    fn test(&self, key: usize) -> bool {
        self.sub[key / T::CAP].test(key % T::CAP)
    }
    fn next(&self, key: usize) -> Option<usize> {
        let ind = key / T::CAP;
        if ind < 16 && self.bitset.get_bit(ind) {
            if let Some(res) = self.sub[ind].next(key - T::CAP * ind) {
                return Some(res).map(|x| x + T::CAP * ind);
            }
        }
        (ind + 1..16).find_map(|i| {
            if self.bitset.get_bit(i) {
                self.sub[i].next(0).map(|x| x + T::CAP * i)
            } else {
                None
            }
        })
    }
}

impl<T: BitAlloc> BitAllocCascade16<T> {
    fn for_range(&mut self, range: Range<usize>, f: impl Fn(&mut T, Range<usize>)) {
        let Range { start, end } = range;
        assert!(start <= end);
        assert!(end <= Self::CAP);
        for i in start / T::CAP..=(end - 1) / T::CAP {
            let begin = if start / T::CAP == i {
                start % T::CAP
            } else {
                0
            };
            let end = if end / T::CAP == i {
                end % T::CAP
            } else {
                T::CAP
            };
            f(&mut self.sub[i], begin..end);
            self.bitset.set_bit(i, self.sub[i].any());
        }
    }
}

/// A bitmap consisting of only 16 bits.
/// BitAlloc16 acts as the leaf (except the leaf bits of course) nodes
/// in the segment trees.
#[derive(Default)]
pub struct BitAlloc16(u16);

impl BitAlloc for BitAlloc16 {
    const CAP: usize = 16;

    const DEFAULT: Self = BitAlloc16(0);

    fn alloc(&mut self) -> Option<usize> {
        if self.any() {
            let i = log2(self.0);
            self.0.set_bit(i, false);
            Some(i)
        } else {
            None
        }
    }
    fn alloc_contiguous(&mut self, size: usize, align_log2: usize) -> Option<usize> {
        if let Some(base) = find_contiguous(self, Self::CAP, size, align_log2) {
            self.remove(base..base + size);
            Some(base)
        } else {
            None
        }
    }
    fn dealloc(&mut self, key: usize) {
        assert!(!self.test(key));
        self.0.set_bit(key, true);
    }
    fn insert(&mut self, range: Range<usize>) {
        self.0.set_bits(range.clone(), 0xffff.get_bits(range));
    }
    fn remove(&mut self, range: Range<usize>) {
        self.0.set_bits(range, 0);
    }
    fn any(&self) -> bool {
        self.0 != 0
    }
    fn test(&self, key: usize) -> bool {
        self.0.get_bit(key)
    }
    fn next(&self, key: usize) -> Option<usize> {
        for i in key..16 {
            if self.0.get_bit(i) {
                return Some(i);
            }
        }
        None
    }
}

fn find_contiguous<T: BitAlloc>(
    ba: &T,
    capacity: usize,
    size: usize,
    align_log2: usize,
) -> Option<usize> {
    if capacity < (1 << align_log2) || !ba.any() {
        None
    } else {
        let mut base = 0;
        let mut offset = base;
        while offset < capacity {
            if let Some(next) = ba.next(offset) {
                if next != offset {
                    // it can be guarenteed that no bit in (offset..next) is free
                    // move to next aligned position after next-1
                    assert!(next > offset);
                    base = (((next - 1) >> align_log2) + 1) << align_log2;
                    assert_ne!(offset, next);
                    offset = base;
                    continue;
                }
            } else {
                return None;
            }
            offset += 1;
            if offset - base == size {
                return Some(base);
            }
        }
        None
    }
}

#[inline(always)]
#[cfg(target_arch = "x86_64")]
fn log2(x: u16) -> usize {
    assert_ne!(x, 0);
    let pos: u16;
    unsafe { asm!("bsrw $1, $0" :"=r"(pos) :"r"(x) : :"volatile") };
    pos as usize
}

#[inline(always)]
#[cfg(not(target_arch = "x86_64"))]
fn log2(x: u16) -> usize {
    log2_naive(x)
}

#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn log2_naive(mut x: u16) -> usize {
    //a naive implement
    assert_ne!(x, 0);
    let mut pos = -1;
    while x != 0 {
        pos += 1;
        x >>= 1;
    }
    pos as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "x86_64"))]
    #[test]
    fn log2_() {
        for x in 1..=0xffff {
            assert_eq!(log2(x), log2_naive(x), "log2 failed: {}", x);
        }
    }

    #[test]
    fn bitalloc16() {
        let mut ba = BitAlloc16::default();
        assert_eq!(BitAlloc16::CAP, 16);
        ba.insert(0..16);
        for i in 0..16 {
            assert_eq!(ba.test(i), true);
        }
        ba.remove(8..14);
        assert_eq!(ba.alloc(), Some(15));
        assert_eq!(ba.alloc(), Some(14));
        assert_eq!(ba.alloc(), Some(7));
        ba.dealloc(14);
        ba.dealloc(15);
        ba.dealloc(7);

        for _ in 0..10 {
            assert!(ba.alloc().is_some());
        }
        assert!(!ba.any());
        assert!(ba.alloc().is_none());
    }

    #[test]
    fn bitalloc4k() {
        let mut ba = BitAlloc4K::default();
        assert_eq!(BitAlloc4K::CAP, 4096);
        ba.insert(0..4096);
        for i in 0..4096 {
            assert_eq!(ba.test(i), true);
        }
        ba.remove(8..4094);
        for i in 0..4096 {
            assert_eq!(ba.test(i), i < 8 || i >= 4094);
        }
        assert_eq!(ba.alloc(), Some(4095));
        assert_eq!(ba.alloc(), Some(4094));
        assert_eq!(ba.alloc(), Some(7));
        ba.dealloc(4095);
        ba.dealloc(4094);
        ba.dealloc(7);

        for _ in 0..10 {
            assert!(ba.alloc().is_some());
        }
        assert!(ba.alloc().is_none());
    }

    #[test]
    fn bitalloc_contiguous() {
        let mut ba0 = BitAlloc16::default();
        ba0.insert(0..BitAlloc16::CAP);
        ba0.remove(3..6);
        assert_eq!(ba0.next(0), Some(0));
        assert_eq!(ba0.alloc_contiguous(1, 1), Some(0));
        assert_eq!(find_contiguous(&ba0, BitAlloc4K::CAP, 2, 0), Some(1));

        let mut ba = BitAlloc4K::default();
        assert_eq!(BitAlloc4K::CAP, 4096);
        ba.insert(0..BitAlloc4K::CAP);
        ba.remove(3..6);
        assert_eq!(ba.next(0), Some(0));
        assert_eq!(ba.alloc_contiguous(1, 1), Some(0));
        assert_eq!(ba.next(0), Some(1));
        assert_eq!(ba.next(1), Some(1));
        assert_eq!(ba.next(2), Some(2));
        assert_eq!(find_contiguous(&ba, BitAlloc4K::CAP, 2, 0), Some(1));
        assert_eq!(ba.alloc_contiguous(2, 0), Some(1));
        assert_eq!(ba.alloc_contiguous(2, 3), Some(8));
        ba.remove(0..4096 - 64);
        assert_eq!(ba.alloc_contiguous(128, 7), None);
        assert_eq!(ba.alloc_contiguous(7, 3), Some(4096 - 64));
        ba.insert(321..323);
        assert_eq!(ba.alloc_contiguous(2, 1), Some(4096 - 64 + 8));
        assert_eq!(ba.alloc_contiguous(2, 0), Some(321));
        assert_eq!(ba.alloc_contiguous(64, 6), None);
        assert_eq!(ba.alloc_contiguous(32, 4), Some(4096 - 48));
        for i in 0..4096 - 64 + 7 {
            ba.dealloc(i);
        }
        for i in 4096 - 64 + 8..4096 - 64 + 10 {
            ba.dealloc(i);
        }
        for i in 4096 - 48..4096 - 16 {
            ba.dealloc(i);
        }
        // for i in 4096 - 32..4096 {
        //     ba.dealloc(i);
        // }
    }

    // #[test]
    // fn bitallocContPerformance() {
    //     let mut ba = Box::new(BitAlloc256M::default());
    //     assert_eq!(BitAlloc256M::CAP, 1 << 28);
    //     ba.insert(0..BitAlloc256M::CAP);
    //     assert_eq!(ba.alloc_contiguous(1 << 20, 20), Some(0));
    //     assert_eq!(ba.alloc_contiguous(1 << 19, 19), Some(1 << 20));
    //     assert_eq!(ba.alloc_contiguous(1 << 21, 21), Some(1 << 21));
    //     assert_eq!(ba.alloc_contiguous(1 << 19, 19), Some(3 << 19));
    // }
}
