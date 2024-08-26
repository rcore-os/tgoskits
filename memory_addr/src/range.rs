use core::{fmt, ops::Range};

use crate::{MemoryAddr, PhysAddr, VirtAddr};

/// A range of a given memory address type `A`.
///
/// The range is inclusive on the start and exclusive on the end. A range is
/// considered **empty** iff `start == end`, and **invalid** iff `start > end`.
/// An invalid range should not be created and cannot be obtained without unsafe
/// operations, calling methods on an invalid range will cause unexpected
/// consequences.
///
/// # Example
///
/// ```
/// use memory_addr::AddrRange;
///
/// let range = AddrRange::<usize>::new(0x1000, 0x2000);
/// assert_eq!(range.start, 0x1000);
/// assert_eq!(range.end, 0x2000);
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AddrRange<A: MemoryAddr> {
    /// The lower bound of the range (inclusive).
    pub start: A,
    /// The upper bound of the range (exclusive).
    pub end: A,
}

/// Methods for [`AddrRange`].
impl<A> AddrRange<A>
where
    A: MemoryAddr,
{
    /// Creates a new address range from the start and end addresses.
    ///
    /// # Panics
    ///
    /// Panics if `start > end`.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::AddrRange;
    ///
    /// let range = AddrRange::new(0x1000usize, 0x2000);
    /// assert_eq!(range.start, 0x1000);
    /// assert_eq!(range.end, 0x2000);
    /// ```
    ///
    /// And this will panic:
    ///
    /// ```should_panic
    /// # use memory_addr::AddrRange;
    /// let _ = AddrRange::new(0x2000usize, 0x1000);
    /// ```
    #[inline]
    pub fn new(start: A, end: A) -> Self {
        assert!(
            start <= end,
            "invalid `AddrRange`: {}..{}",
            start.into(),
            end.into()
        );
        Self { start, end }
    }

    /// Creates a new address range from the given range.
    ///
    /// Returns `None` if `start > end`.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::AddrRange;
    ///
    /// let range = AddrRange::try_new(0x1000usize, 0x2000).unwrap();
    /// assert_eq!(range.start, 0x1000);
    /// assert_eq!(range.end, 0x2000);
    /// assert!(AddrRange::try_new(0x2000usize, 0x1000).is_none());
    /// ```
    #[inline]
    pub fn try_new(start: A, end: A) -> Option<Self> {
        if start <= end {
            Some(Self { start, end })
        } else {
            None
        }
    }

    /// Creates a new address range from the given range without checking the
    /// validity.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `start <= end`, otherwise the range will be
    /// invalid and unexpected consequences will occur.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::AddrRange;
    ///
    /// let range = unsafe { AddrRange::new_unchecked(0x1000usize, 0x2000) };
    /// assert_eq!(range.start, 0x1000);
    /// assert_eq!(range.end, 0x2000);
    /// ```
    #[inline]
    pub const unsafe fn new_unchecked(start: A, end: A) -> Self {
        Self { start, end }
    }

    /// Creates a new address range from the start address and the size.
    ///
    /// # Panics
    ///
    /// Panics if `size` is too large and causes overflow during evaluating the
    /// end address.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::AddrRange;
    ///
    /// let range = AddrRange::from_start_size(0x1000usize, 0x1000);
    /// assert_eq!(range.start, 0x1000);
    /// assert_eq!(range.end, 0x2000);
    /// ```
    ///
    /// And this will panic:
    ///
    /// ```should_panic
    /// # use memory_addr::AddrRange;
    /// let _ = AddrRange::from_start_size(0x1000usize, usize::MAX);
    /// ```
    #[inline]
    pub fn from_start_size(start: A, size: usize) -> Self {
        if let Some(end) = start.checked_add(size) {
            Self { start, end }
        } else {
            panic!(
                "size too large for `AddrRange`: {} + {}",
                start.into(),
                size
            );
        }
    }

    /// Creates a new address range from the start address and the size.
    ///
    /// Returns `None` if `size` is too large and causes overflow during
    /// evaluating the end address.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::AddrRange;
    ///
    /// let range = AddrRange::try_from_start_size(0x1000usize, 0x1000).unwrap();
    /// assert_eq!(range.start, 0x1000);
    /// assert_eq!(range.end, 0x2000);
    /// assert!(AddrRange::try_from_start_size(0x1000usize, usize::MAX).is_none());
    /// ```
    #[inline]
    pub fn try_from_start_size(start: A, size: usize) -> Option<Self> {
        start.checked_add(size).map(|end| Self { start, end })
    }

    /// Creates a new address range from the start address and the size without
    /// checking the validity.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `size` is not too large and won't cause
    /// overflow during evaluating the end address. Failing to do so will
    /// create an invalid range and cause unexpected consequences.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::AddrRange;
    ///
    /// let range = unsafe { AddrRange::from_start_size_unchecked(0x1000usize, 0x1000) };
    /// assert_eq!(range.start, 0x1000);
    /// assert_eq!(range.end, 0x2000);
    /// ```
    #[inline]
    pub unsafe fn from_start_size_unchecked(start: A, size: usize) -> Self {
        Self {
            start,
            end: start.wrapping_add(size),
        }
    }

    /// Returns `true` if the range is empty.
    ///
    /// It's also guaranteed that `false` will be returned if the range is
    /// invalid (i.e., `start > end`).
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::AddrRange;
    ///
    /// assert!(AddrRange::new(0x1000usize, 0x1000).is_empty());
    /// assert!(!AddrRange::new(0x1000usize, 0x2000).is_empty());
    /// ```
    #[inline]
    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }

    /// Returns the size of the range.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::AddrRange;
    ///
    /// assert_eq!(AddrRange::new(0x1000usize, 0x1000).size(), 0);
    /// assert_eq!(AddrRange::new(0x1000usize, 0x2000).size(), 0x1000);
    /// ```
    #[inline]
    pub fn size(self) -> usize {
        self.end.wrapping_sub_addr(self.start)
    }

    /// Checks if the range contains the given address.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::AddrRange;
    ///
    /// let range = AddrRange::new(0x1000usize, 0x2000);
    /// assert!(!range.contains(0x0fff));
    /// assert!(range.contains(0x1000));
    /// assert!(range.contains(0x1fff));
    /// assert!(!range.contains(0x2000));
    /// ```
    #[inline]
    pub fn contains(self, addr: A) -> bool {
        self.start <= addr && addr < self.end
    }

    /// Checks if the range contains the given address range.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::{addr_range, AddrRange};
    ///
    /// let range = AddrRange::new(0x1000usize, 0x2000);
    /// assert!(!range.contains_range(addr_range!(0x0usize..0xfff)));
    /// assert!(!range.contains_range(addr_range!(0x0fffusize..0x1fff)));
    /// assert!(range.contains_range(addr_range!(0x1001usize..0x1fff)));
    /// assert!(range.contains_range(addr_range!(0x1000usize..0x2000)));
    /// assert!(!range.contains_range(addr_range!(0x1001usize..0x2001)));
    /// assert!(!range.contains_range(addr_range!(0x2001usize..0x3001)));
    /// ```
    #[inline]
    pub fn contains_range(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// Checks if the range is contained in the given address range.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::{addr_range, AddrRange};
    ///
    /// let range = AddrRange::new(0x1000usize, 0x2000);
    /// assert!(!range.contained_in(addr_range!(0xfffusize..0x1fff)));
    /// assert!(!range.contained_in(addr_range!(0x1001usize..0x2001)));
    /// assert!(range.contained_in(addr_range!(0xfffusize..0x2001)));
    /// assert!(range.contained_in(addr_range!(0x1000usize..0x2000)));
    /// ```
    #[inline]
    pub fn contained_in(self, other: Self) -> bool {
        other.contains_range(self)
    }

    /// Checks if the range overlaps with the given address range.
    ///
    /// # Example
    ///
    /// ```
    /// use memory_addr::{addr_range, AddrRange};
    ///
    /// let range = AddrRange::new(0x1000usize, 0x2000usize);
    /// assert!(!range.overlaps(addr_range!(0xfffusize..0xfff)));
    /// assert!(!range.overlaps(addr_range!(0x2000usize..0x2000)));
    /// assert!(!range.overlaps(addr_range!(0xfffusize..0x1000)));
    /// assert!(range.overlaps(addr_range!(0xfffusize..0x1001)));
    /// assert!(range.overlaps(addr_range!(0x1fffusize..0x2001)));
    /// assert!(range.overlaps(addr_range!(0xfffusize..0x2001)));
    /// ```
    #[inline]
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Conversion from [`Range`] to [`AddrRange`], provided that the type of the
/// endpoints can be converted to the address type `A`.
impl<A, T> TryFrom<Range<T>> for AddrRange<A>
where
    A: MemoryAddr + From<T>,
{
    type Error = ();

    #[inline]
    fn try_from(range: Range<T>) -> Result<Self, Self::Error> {
        Self::try_new(range.start.into(), range.end.into()).ok_or(())
    }
}

/// Implementations of [`Default`] for [`AddrRange`].
///
/// The default value is an empty range `Range { start: 0, end: 0 }`.
impl<A> Default for AddrRange<A>
where
    A: MemoryAddr,
{
    #[inline]
    fn default() -> Self {
        Self {
            start: 0.into(),
            end: 0.into(),
        }
    }
}

/// Implementations of [`Debug`](fmt::Debug) for [`AddrRange`].
impl<A> fmt::Debug for AddrRange<A>
where
    A: MemoryAddr + fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}..{:?}", self.start, self.end)
    }
}

/// Implementations of [`LowerHex`](fmt::LowerHex) for [`AddrRange`].
impl<A> fmt::LowerHex for AddrRange<A>
where
    A: MemoryAddr + fmt::LowerHex,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:x}..{:x}", self.start, self.end)
    }
}

/// Implementations of [`UpperHex`](fmt::UpperHex) for [`AddrRange`].
impl<A> fmt::UpperHex for AddrRange<A>
where
    A: MemoryAddr + fmt::UpperHex,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:X}..{:X}", self.start, self.end)
    }
}

/// A range of virtual addresses [`VirtAddr`].
pub type VirtAddrRange = AddrRange<VirtAddr>;
/// A range of physical addresses [`PhysAddr`].
pub type PhysAddrRange = AddrRange<PhysAddr>;

/// Converts the given range expression into [`AddrRange`]. Panics if the range
/// is invalid.
///
/// The concrete address type is inferred from the context.
///
/// # Example
///
/// ```
/// use memory_addr::{addr_range, AddrRange};
///
/// let range: AddrRange<usize> = addr_range!(0x1000usize..0x2000);
/// assert_eq!(range.start, 0x1000usize);
/// assert_eq!(range.end, 0x2000usize);
/// ```
///
/// And this will panic:
///
/// ```should_panic
/// # use memory_addr::{addr_range, AddrRange};
/// let _: AddrRange<usize> = addr_range!(0x2000usize..0x1000);
/// ```
#[macro_export]
macro_rules! addr_range {
    ($range:expr) => {
        $crate::AddrRange::try_from($range).expect("invalid address range in `addr_range!`")
    };
}

/// Converts the given range expression into [`VirtAddrRange`]. Panics if the
/// range is invalid.
///
/// # Example
///
/// ```
/// use memory_addr::va_range;
///
/// let range = va_range!(0x1000..0x2000);
/// assert_eq!(range.start, 0x1000.into());
/// assert_eq!(range.end, 0x2000.into());
/// ```
///
/// And this will panic:
///
/// ```should_panic
/// # use memory_addr::va_range;
/// let _ = va_range!(0x2000..0x1000);
/// ```
#[macro_export]
macro_rules! va_range {
    ($range:expr) => {
        $crate::VirtAddrRange::try_from($range).expect("invalid address range in `va_range!`")
    };
}

/// Converts the given range expression into [`PhysAddrRange`]. Panics if the
/// range is invalid.
///
/// # Example
///
/// ```
/// use memory_addr::pa_range;
///
/// let range = pa_range!(0x1000..0x2000);
/// assert_eq!(range.start, 0x1000.into());
/// assert_eq!(range.end, 0x2000.into());
/// ```
///
/// And this will panic:
///
/// ```should_panic
/// # use memory_addr::pa_range;
/// let _ = pa_range!(0x2000..0x1000);
/// ```
#[macro_export]
macro_rules! pa_range {
    ($range:expr) => {
        $crate::PhysAddrRange::try_from($range).expect("invalid address range in `pa_range!`")
    };
}

#[cfg(test)]
mod test {
    use crate::{va, va_range, VirtAddrRange};

    #[test]
    fn test_range_format() {
        let range = va_range!(0xfec000..0xfff000usize);

        assert_eq!(format!("{:?}", range), "VA:0xfec000..VA:0xfff000");
        assert_eq!(format!("{:x}", range), "VA:0xfec000..VA:0xfff000");
        assert_eq!(format!("{:X}", range), "VA:0xFEC000..VA:0xFFF000");
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

        assert!(!range.contains_range(addr_range!(0xfff..0x1fff)));
        assert!(!range.contains_range(addr_range!(0xfff..0x2000)));
        assert!(!range.contains_range(va_range!(0xfff..0x2001))); // test both `va_range!` and `addr_range!`
        assert!(range.contains_range(va_range!(0x1000..0x1fff)));
        assert!(range.contains_range(addr_range!(0x1000..0x2000)));
        assert!(!range.contains_range(addr_range!(0x1000..0x2001)));
        assert!(range.contains_range(va_range!(0x1001..0x1fff)));
        assert!(range.contains_range(va_range!(0x1001..0x2000)));
        assert!(!range.contains_range(va_range!(0x1001..0x2001)));
        assert!(!range.contains_range(VirtAddrRange::from_start_size(0xfff.into(), 0x1)));
        assert!(!range.contains_range(VirtAddrRange::from_start_size(0x2000.into(), 0x1)));

        assert!(range.contained_in(addr_range!(0xfff..0x2000)));
        assert!(range.contained_in(addr_range!(0x1000..0x2000)));
        assert!(range.contained_in(va_range!(0x1000..0x2001)));

        assert!(!range.overlaps(addr_range!(0x800..0x1000)));
        assert!(range.overlaps(addr_range!(0x800..0x1001)));
        assert!(range.overlaps(addr_range!(0x1800..0x2000)));
        assert!(range.overlaps(va_range!(0x1800..0x2001)));
        assert!(!range.overlaps(va_range!(0x2000..0x2800)));
        assert!(range.overlaps(va_range!(0xfff..0x2001)));

        let default_range: VirtAddrRange = Default::default();
        assert!(default_range.is_empty());
        assert_eq!(default_range.size(), 0);
        assert_eq!(default_range.start, va!(0));
        assert_eq!(default_range.end, va!(0));
    }
}
