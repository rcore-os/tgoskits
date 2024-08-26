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
    use super::*;

    #[test]
    fn test_align() {
        assert_eq!(align_down(0x12345678, 0x1000), 0x12345000);
        assert_eq!(align_up(0x12345678, 0x1000), 0x12346000);
        assert_eq!(align_offset(0x12345678, 0x1000), 0x678);
        assert!(is_aligned(0x12345000, 0x1000));
        assert!(!is_aligned(0x12345678, 0x1000));

        assert_eq!(align_down_4k(0x12345678), 0x12345000);
        assert_eq!(align_up_4k(0x12345678), 0x12346000);
        assert_eq!(align_offset_4k(0x12345678), 0x678);
        assert!(is_aligned_4k(0x12345000));
        assert!(!is_aligned_4k(0x12345678));
    }
}
