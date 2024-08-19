#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

mod addr;
mod iter;
mod range;

pub use self::addr::{MemoryAddr, PhysAddr, VirtAddr};
pub use self::iter::PageIter;
pub use self::range::{AddrRange, PhysAddrRange, VirtAddrRange};

/// The size of a 4K page (4096 bytes).
pub const PAGE_SIZE_4K: usize = 0x1000;

/// A [`PageIter`] for 4K pages.
pub type PageIter4K<A> = PageIter<PAGE_SIZE_4K, A>;

/// Align address downwards.
///
/// Returns the greatest `x` with alignment `align` so that `x <= addr`.
///
/// The alignment must be a power of two.
#[inline]
pub const fn align_down(addr: usize, align: usize) -> usize {
    addr & !(align - 1)
}

/// Align address upwards.
///
/// Returns the smallest `x` with alignment `align` so that `x >= addr`.
///
/// The alignment must be a power of two.
#[inline]
pub const fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

/// Returns the offset of the address within the alignment.
///
/// Equivalent to `addr % align`, but the alignment must be a power of two.
#[inline]
pub const fn align_offset(addr: usize, align: usize) -> usize {
    addr & (align - 1)
}

/// Checks whether the address has the demanded alignment.
///
/// Equivalent to `addr % align == 0`, but the alignment must be a power of two.
#[inline]
pub const fn is_aligned(addr: usize, align: usize) -> bool {
    align_offset(addr, align) == 0
}

/// Align address downwards to 4096 (bytes).
#[inline]
pub const fn align_down_4k(addr: usize) -> usize {
    align_down(addr, PAGE_SIZE_4K)
}

/// Align address upwards to 4096 (bytes).
#[inline]
pub const fn align_up_4k(addr: usize) -> usize {
    align_up(addr, PAGE_SIZE_4K)
}

/// Returns the offset of the address within a 4K-sized page.
#[inline]
pub const fn align_offset_4k(addr: usize) -> usize {
    align_offset(addr, PAGE_SIZE_4K)
}

/// Checks whether the address is 4K-aligned.
#[inline]
pub const fn is_aligned_4k(addr: usize) -> bool {
    is_aligned(addr, PAGE_SIZE_4K)
}

#[cfg(test)]
mod tests {
    use crate::{va, va_range, VirtAddrRange};

    #[test]
    fn test_addr() {
        let addr = va!(0x2000);
        assert!(addr.is_aligned_4k());
        assert!(!addr.is_aligned(0x10000usize));
        assert_eq!(addr.align_offset_4k(), 0);
        assert_eq!(addr.align_down_4k(), va!(0x2000));
        assert_eq!(addr.align_up_4k(), va!(0x2000));

        let addr = va!(0x2fff);
        assert!(!addr.is_aligned_4k());
        assert_eq!(addr.align_offset_4k(), 0xfff);
        assert_eq!(addr.align_down_4k(), va!(0x2000));
        assert_eq!(addr.align_up_4k(), va!(0x3000));

        let align = 0x100000;
        let addr = va!(align * 5) + 0x2000;
        assert!(addr.is_aligned_4k());
        assert!(!addr.is_aligned(align));
        assert_eq!(addr.align_offset(align), 0x2000);
        assert_eq!(addr.align_down(align), va!(align * 5));
        assert_eq!(addr.align_up(align), va!(align * 6));
    }

    #[test]
    fn test_range() {
        let start = va!(0x1000);
        let end = va!(0x2000);
        let range = va_range!(start..end);
        println!("range: {:?}", range);

        assert!((0x1000..0x1000).is_empty());
        assert!((0x1000..0xfff).is_empty());
        assert!(!range.is_empty());

        assert_eq!(range.start, start);
        assert_eq!(range.end, end);
        assert_eq!(range.size(), 0x1000);

        assert!(range.contains(va!(0x1000)));
        assert!(range.contains(va!(0x1080)));
        assert!(!range.contains(va!(0x2000)));

        assert!(!range.contains_range((0xfff..0x1fff).into()));
        assert!(!range.contains_range((0xfff..0x2000).into()));
        assert!(!range.contains_range((0xfff..0x2001).into()));
        assert!(range.contains_range((0x1000..0x1fff).into()));
        assert!(range.contains_range((0x1000..0x2000).into()));
        assert!(!range.contains_range((0x1000..0x2001).into()));
        assert!(range.contains_range((0x1001..0x1fff).into()));
        assert!(range.contains_range((0x1001..0x2000).into()));
        assert!(!range.contains_range((0x1001..0x2001).into()));
        assert!(!range.contains_range(VirtAddrRange::from_start_size(0xfff.into(), 0x1)));
        assert!(!range.contains_range(VirtAddrRange::from_start_size(0x2000.into(), 0x1)));

        assert!(range.contained_in((0xfff..0x2000).into()));
        assert!(range.contained_in((0x1000..0x2000).into()));
        assert!(range.contained_in((0x1000..0x2001).into()));

        assert!(!range.overlaps((0x800..0x1000).into()));
        assert!(range.overlaps((0x800..0x1001).into()));
        assert!(range.overlaps((0x1800..0x2000).into()));
        assert!(range.overlaps((0x1800..0x2001).into()));
        assert!(!range.overlaps((0x2000..0x2800).into()));
        assert!(range.overlaps((0xfff..0x2001).into()));
    }
}
