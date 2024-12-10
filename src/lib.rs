#![no_std]
#![doc = include_str!("../README.md")]

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
    ///
    /// If `base` is not `None`, the allocator will try to allocate the block at the given base.
    fn alloc_contiguous(
        &mut self,
        base: Option<usize>,
        size: usize,
        align_log2: usize,
    ) -> Option<usize>;

    /// Find a index not less than a given key, where the bit is free.
    fn next(&self, key: usize) -> Option<usize>;

    /// Free an allocated bit.
    ///
    /// Returns true if successful, false if the bit is already free.
    fn dealloc(&mut self, key: usize) -> bool;

    /// Free a contiguous block of bits.
    ///
    /// Returns true if successful, false if the bits in the range are already free.
    fn dealloc_contiguous(&mut self, base: usize, size: usize) -> bool;

    /// Mark bits in the range as unallocated (available)
    fn insert(&mut self, range: Range<usize>);

    /// Reverse of insert
    fn remove(&mut self, range: Range<usize>);

    /// Whether there are free bits remaining
    #[deprecated = "use `!self.is_empty()` instead"]
    fn any(&self) -> bool;

    /// Returns true if no bits is available.
    fn is_empty(&self) -> bool;

    /// Whether a specific bit is free
    fn test(&self, key: usize) -> bool;
}

/// A bitmap of 256 bits
///
/// ## Example
///
/// ```rust
/// use bitmap_allocator::{BitAlloc, BitAlloc256};
///
/// let mut ba = BitAlloc256::default();
/// ba.insert(0..16);
/// for i in 0..16 {
///     assert!(ba.test(i));
/// }
/// ba.remove(2..8);
/// assert_eq!(ba.alloc(), Some(0));
/// assert_eq!(ba.alloc(), Some(1));
/// assert_eq!(ba.alloc(), Some(8));
/// ba.dealloc(0);
/// ba.dealloc(1);
/// ba.dealloc(8);
///
/// assert!(!ba.is_empty());
/// ```
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
    /// for each bit, 1 indicates available, 0 indicates inavailable
    bitset: u16,
    sub: [T; 16],
}

impl<T: BitAlloc> BitAlloc for BitAllocCascade16<T> {
    const CAP: usize = T::CAP * 16;

    const DEFAULT: Self = BitAllocCascade16 {
        bitset: 0,
        sub: [T::DEFAULT; 16],
    };

    fn alloc(&mut self) -> Option<usize> {
        if !self.is_empty() {
            let i = self.bitset.trailing_zeros() as usize;
            let res = self.sub[i].alloc().unwrap() + i * T::CAP;
            self.bitset.set_bit(i, !self.sub[i].is_empty());
            Some(res)
        } else {
            None
        }
    }

    fn alloc_contiguous(
        &mut self,
        base: Option<usize>,
        size: usize,
        align_log2: usize,
    ) -> Option<usize> {
        match base {
            Some(base) => check_contiguous(self, base, Self::CAP, size, align_log2).then(|| {
                self.remove(base..base + size);
                base
            }),
            None => find_contiguous(self, Self::CAP, size, align_log2).inspect(|&base| {
                self.remove(base..base + size);
            }),
        }
    }

    fn dealloc(&mut self, key: usize) -> bool {
        let i = key / T::CAP;
        self.bitset.set_bit(i, true);
        self.sub[i].dealloc(key % T::CAP)
    }

    fn dealloc_contiguous(&mut self, base: usize, size: usize) -> bool {
        let mut success = true;
        let Range { start, end } = base..base + size;

        // Check if the range is valid.
        if end > Self::CAP {
            return false;
        }

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
            success = success && self.sub[i].dealloc_contiguous(begin, end - begin);
            self.bitset.set_bit(i, !self.sub[i].is_empty());
        }
        success
    }

    fn insert(&mut self, range: Range<usize>) {
        self.for_range(range, |sub: &mut T, range| sub.insert(range));
    }
    fn remove(&mut self, range: Range<usize>) {
        self.for_range(range, |sub: &mut T, range| sub.remove(range));
    }
    fn any(&self) -> bool {
        !self.is_empty()
    }
    fn is_empty(&self) -> bool {
        self.bitset == 0
    }
    fn test(&self, key: usize) -> bool {
        self.sub[key / T::CAP].test(key % T::CAP)
    }
    fn next(&self, key: usize) -> Option<usize> {
        let idx = key / T::CAP;
        (idx..16).find_map(|i| {
            if self.bitset.get_bit(i) {
                let key = if i == idx { key - T::CAP * idx } else { 0 };
                self.sub[i].next(key).map(|x| x + T::CAP * i)
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
            self.bitset.set_bit(i, !self.sub[i].is_empty());
        }
    }
}

/// A bitmap consisting of only 16 bits.
/// BitAlloc16 acts as the leaf (except the leaf bits of course) nodes in the segment trees.
///
/// ## Example
///
/// ```rust
/// use bitmap_allocator::{BitAlloc, BitAlloc16};
///
/// let mut ba = BitAlloc16::default();
/// assert_eq!(BitAlloc16::CAP, 16);
/// ba.insert(0..16);
/// for i in 0..16 {
///     assert!(ba.test(i));
/// }
/// ba.remove(2..8);
/// assert_eq!(ba.alloc(), Some(0));
/// assert_eq!(ba.alloc(), Some(1));
/// assert_eq!(ba.alloc(), Some(8));
/// ba.dealloc(0);
/// ba.dealloc(1);
/// ba.dealloc(8);
///
/// assert!(!ba.is_empty());
/// ```
#[derive(Default)]
pub struct BitAlloc16(u16);

impl BitAlloc for BitAlloc16 {
    const CAP: usize = u16::BITS as usize;

    const DEFAULT: Self = Self(0);

    fn alloc(&mut self) -> Option<usize> {
        let i = self.0.trailing_zeros() as usize;
        if i < Self::CAP {
            self.0.set_bit(i, false);
            Some(i)
        } else {
            None
        }
    }
    fn alloc_contiguous(
        &mut self,
        base: Option<usize>,
        size: usize,
        align_log2: usize,
    ) -> Option<usize> {
        match base {
            Some(base) => check_contiguous(self, base, Self::CAP, size, align_log2).then(|| {
                self.remove(base..base + size);
                base
            }),
            None => find_contiguous(self, Self::CAP, size, align_log2).inspect(|&base| {
                self.remove(base..base + size);
            }),
        }
    }

    fn dealloc(&mut self, key: usize) -> bool {
        let success = !self.test(key);
        self.0.set_bit(key, true);
        success
    }

    fn dealloc_contiguous(&mut self, base: usize, size: usize) -> bool {
        if self.0.get_bits(base..base + size) == 0 {
            self.insert(base..base + size);
            return true;
        }
        false
    }

    fn insert(&mut self, range: Range<usize>) {
        self.0.set_bits(range.clone(), 0xffff.get_bits(range));
    }
    fn remove(&mut self, range: Range<usize>) {
        self.0.set_bits(range, 0);
    }
    fn any(&self) -> bool {
        !self.is_empty()
    }
    fn is_empty(&self) -> bool {
        self.0 == 0
    }
    fn test(&self, key: usize) -> bool {
        self.0.get_bit(key)
    }
    fn next(&self, key: usize) -> Option<usize> {
        (key..Self::CAP).find(|&i| self.0.get_bit(i))
    }
}

fn find_contiguous(
    ba: &impl BitAlloc,
    capacity: usize,
    size: usize,
    align_log2: usize,
) -> Option<usize> {
    if capacity < (1 << align_log2) || ba.is_empty() {
        return None;
    }

    let mut base = 0;
    // First, we need to make sure that base is aligned.
    if let Some(start) = ba.next(base) {
        base = align_up_log2(start, align_log2);
    } else {
        return None;
    }

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

fn check_contiguous(
    ba: &impl BitAlloc,
    base: usize,
    capacity: usize,
    size: usize,
    align_log2: usize,
) -> bool {
    if capacity < (1 << align_log2) || ba.is_empty() {
        return false;
    }

    // First, we need to make sure that base is aligned.
    if !is_aligned_log2(base, align_log2) {
        return false;
    }

    let mut offset = base;
    while offset < capacity {
        if let Some(next) = ba.next(offset) {
            if next != offset {
                return false;
            }
            offset += 1;
            if offset - base == size {
                return true;
            }
        } else {
            return false;
        }
    }
    false
}

fn align_up_log2(base: usize, align_log2: usize) -> usize {
    (base + ((1 << align_log2) - 1)) & !((1 << align_log2) - 1)
}

fn is_aligned_log2(base: usize, align_log2: usize) -> bool {
    (base & ((1 << align_log2) - 1)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitalloc16() {
        let mut ba = BitAlloc16::default();
        assert_eq!(BitAlloc16::CAP, 16);
        ba.insert(0..16);
        for i in 0..16 {
            assert!(ba.test(i));
        }
        ba.remove(2..8);
        assert_eq!(ba.alloc(), Some(0));
        assert_eq!(ba.alloc(), Some(1));
        assert_eq!(ba.alloc(), Some(8));
        ba.dealloc(0);
        ba.dealloc(1);
        ba.dealloc(8);

        assert!(!ba.is_empty());
        for _ in 0..10 {
            assert!(ba.alloc().is_some());
        }
        assert!(ba.is_empty());
        assert!(ba.alloc().is_none());

        for key in 0..16 {
            assert!(ba.dealloc(key));
        }

        assert!(!ba.dealloc(10));
        assert!(!ba.dealloc(0));

        assert_eq!(ba.alloc(), Some(0));
        assert_eq!(ba.test(0), false);
        assert_eq!(ba.alloc_contiguous(None, 2, 0), Some(1));
        assert_eq!(ba.test(1), false);
        assert_eq!(ba.test(2), false);

        // Test alloc alignment.
        assert_eq!(ba.alloc_contiguous(None, 2, 1), Some(4));
        // Bit 3 is free due to alignment.
        assert_eq!(ba.test(3), true);
        assert_eq!(ba.test(4), false);
        assert_eq!(ba.test(5), false);
        assert_eq!(ba.next(5), Some(6));

        // Test alloc alignment.
        assert_eq!(ba.alloc_contiguous(None, 3, 3), Some(8));
        assert_eq!(ba.next(8), Some(11));

        assert_eq!(ba.alloc_contiguous(Some(2), 2, 1), None);
        assert_eq!(ba.alloc_contiguous(Some(6), 2, 1), Some(6));
        assert_eq!(ba.alloc_contiguous(Some(6), 2, 1), None);

        assert!(ba.dealloc_contiguous(8, 3));

        assert_eq!(ba.alloc_contiguous(Some(8), 3, 2), Some(8));
        assert_eq!(ba.next(8), Some(11));
        assert_eq!(ba.alloc_contiguous(Some(11), 1, 0), Some(11));
        assert_eq!(ba.next(11), Some(12));

        assert_eq!(ba.alloc_contiguous(Some(12), 3, 2), Some(12));

        assert_eq!(ba.next(12), Some(15));

        assert_eq!(ba.alloc(), Some(3));
        assert_eq!(ba.alloc(), Some(15));

        assert!(ba.is_empty());
        assert!(ba.alloc().is_none());

        assert!(ba.dealloc_contiguous(6, 2));
        assert!(ba.dealloc_contiguous(8, 3));
        assert!(ba.dealloc_contiguous(11, 1));
        assert!(ba.dealloc_contiguous(12, 3));
    }

    #[test]
    fn bitalloc4k() {
        let mut ba = BitAlloc4K::default();
        assert_eq!(BitAlloc4K::CAP, 4096);
        ba.insert(0..4096);
        for i in 0..4096 {
            assert!(ba.test(i));
        }
        ba.remove(2..4094);
        for i in 0..4096 {
            assert_eq!(ba.test(i), !(2..4094).contains(&i));
        }
        assert_eq!(ba.alloc(), Some(0));
        assert_eq!(ba.alloc(), Some(1));
        assert_eq!(ba.alloc(), Some(4094));
        ba.dealloc(0);
        ba.dealloc(1);
        ba.dealloc(4094);

        assert!(!ba.is_empty());
        for _ in 0..4 {
            assert!(ba.alloc().is_some());
        }
        assert!(ba.is_empty());
        assert!(ba.alloc().is_none());
    }

    #[test]
    fn bitalloc_contiguous() {
        let mut ba0 = BitAlloc16::default();
        ba0.insert(0..BitAlloc16::CAP);
        ba0.remove(3..6);
        assert_eq!(ba0.next(0), Some(0));
        assert_eq!(ba0.alloc_contiguous(None, 1, 1), Some(0));
        assert_eq!(find_contiguous(&ba0, BitAlloc4K::CAP, 2, 0), Some(1));

        let mut ba = BitAlloc4K::default();
        assert_eq!(BitAlloc4K::CAP, 4096);
        ba.insert(0..BitAlloc4K::CAP);
        ba.remove(3..6);
        assert_eq!(ba.next(0), Some(0));
        assert_eq!(ba.alloc_contiguous(None, 1, 1), Some(0));
        assert_eq!(ba.next(0), Some(1));
        assert_eq!(ba.next(1), Some(1));
        assert_eq!(ba.next(2), Some(2));
        assert_eq!(find_contiguous(&ba, BitAlloc4K::CAP, 2, 0), Some(1));
        assert_eq!(ba.alloc_contiguous(None, 2, 0), Some(1));
        assert_eq!(ba.alloc_contiguous(None, 2, 3), Some(8));
        ba.remove(0..4096 - 64);
        assert_eq!(ba.alloc_contiguous(None, 128, 7), None);
        assert_eq!(ba.alloc_contiguous(None, 7, 3), Some(4096 - 64));
        ba.insert(321..323);
        assert_eq!(ba.alloc_contiguous(None, 2, 1), Some(4096 - 64 + 8));
        assert_eq!(ba.alloc_contiguous(None, 2, 0), Some(321));
        assert_eq!(ba.alloc_contiguous(None, 64, 6), None);
        assert_eq!(ba.alloc_contiguous(None, 32, 4), Some(4096 - 48));
        for i in 0..4096 - 64 + 7 {
            assert!(ba.dealloc(i));
        }
        for i in 4096 - 64 + 8..4096 - 64 + 10 {
            assert!(ba.dealloc(i));
        }
        for i in 4096 - 48..4096 - 16 {
            assert!(ba.dealloc(i));
        }
    }
}
