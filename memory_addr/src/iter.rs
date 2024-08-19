use crate::is_aligned;

/// A page-by-page iterator.
///
/// The page size is specified by the generic parameter `PAGE_SIZE`, which must
/// be a power of 2.
///
/// The address type is specified by the type parameter `A`.
///
/// # Examples
///
/// ```
/// use memory_addr::PageIter;
///
/// let mut iter = PageIter::<0x1000, usize>::new(0x1000, 0x3000).unwrap();
/// assert_eq!(iter.next(), Some(0x1000));
/// assert_eq!(iter.next(), Some(0x2000));
/// assert_eq!(iter.next(), None);
/// ```
pub struct PageIter<const PAGE_SIZE: usize, A>
where
    A: Into<usize> + Copy,
{
    start: A,
    end: A,
}

impl<A, const PAGE_SIZE: usize> PageIter<PAGE_SIZE, A>
where
    A: Into<usize> + Copy,
{
    /// Creates a new [`PageIter`].
    ///
    /// Returns `None` if `PAGE_SIZE` is not a power of 2, or `start` or `end`
    /// is not page-aligned.
    pub fn new(start: A, end: A) -> Option<Self> {
        if !PAGE_SIZE.is_power_of_two()
            || !is_aligned(Into::<usize>::into(start), PAGE_SIZE)
            || !is_aligned(Into::<usize>::into(end), PAGE_SIZE)
        {
            None
        } else {
            Some(Self { start, end })
        }
    }
}

impl<A, const PAGE_SIZE: usize> Iterator for PageIter<PAGE_SIZE, A>
where
    A: Into<usize> + Copy + PartialOrd + core::ops::AddAssign<usize>,
{
    type Item = A;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start < self.end {
            let ret = self.start;
            self.start += PAGE_SIZE;
            Some(ret)
        } else {
            None
        }
    }
}
