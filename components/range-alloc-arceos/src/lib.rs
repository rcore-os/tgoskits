//! A simple, fast range allocator for managing contiguous ranges of resources.
//!
//! This crate provides a [`RangeAllocator`] that efficiently allocates and frees
//! contiguous ranges from an initial range. It uses a best-fit allocation strategy
//! to minimize memory fragmentation.
//!
//! # Example
//!
//! ```
//! use range_alloc_arceos::RangeAllocator;
//!
//! let mut allocator = RangeAllocator::new(0..100);
//!
//! // Allocate a range of length 10
//! let range = allocator.allocate_range(10).unwrap();
//! assert_eq!(range, 0..10);
//!
//! // Free the range when done
//! allocator.free_range(range);
//! ```

#![no_std]

extern crate alloc;

use alloc::{vec, vec::Vec};
use core::{
    fmt::Debug,
    iter::Sum,
    ops::{Add, AddAssign, Range, Sub},
};

/// A range allocator that manages allocation and deallocation of contiguous ranges.
///
/// The allocator starts with an initial range and maintains a list of free ranges.
/// It uses a best-fit allocation strategy to minimize fragmentation when allocating
/// new ranges.
///
/// # Type Parameters
///
/// * `T` - The type used for range bounds. Must support arithmetic operations and ordering.
#[derive(Debug)]
pub struct RangeAllocator<T> {
    /// The range this allocator covers.
    initial_range: Range<T>,
    /// A Vec of ranges in this heap which are unused.
    /// Must be ordered with ascending range start to permit short circuiting allocation.
    /// No two ranges in this vec may overlap.
    free_ranges: Vec<Range<T>>,
}

/// Error type returned when a range allocation fails.
///
/// This error indicates that there is not enough contiguous space available
/// to satisfy the allocation request, although there may be enough total free
/// space if it were defragmented.
#[derive(Clone, Debug, PartialEq)]
pub struct RangeAllocationError<T> {
    /// The total length of all free ranges combined.
    ///
    /// This value represents how much space would be available if all fragmented
    /// free ranges could be combined into one contiguous range.
    pub fragmented_free_length: T,
}

impl<T> RangeAllocator<T>
where
    T: Clone + Copy + Add<Output = T> + AddAssign + Sub<Output = T> + Eq + PartialOrd + Debug,
{
    /// Creates a new range allocator with the specified initial range.
    ///
    /// The entire initial range is marked as free and available for allocation.
    ///
    /// # Arguments
    ///
    /// * `range` - The initial range that this allocator will manage.
    ///
    /// # Example
    ///
    /// ```
    /// use range_alloc_arceos::RangeAllocator;
    ///
    /// let allocator = RangeAllocator::new(0..1024);
    /// ```
    pub fn new(range: Range<T>) -> Self {
        RangeAllocator {
            initial_range: range.clone(),
            free_ranges: vec![range],
        }
    }

    /// Returns a reference to the initial range managed by this allocator.
    ///
    /// This is the range that was provided when the allocator was created,
    /// or the expanded range if [`grow_to`](Self::grow_to) was called.
    pub fn initial_range(&self) -> &Range<T> {
        &self.initial_range
    }

    /// Grows the allocator's range to a new end point.
    ///
    /// This extends the upper bound of the initial range and makes the new space
    /// available for allocation. If the last free range ends at the current upper
    /// bound, it is extended; otherwise, a new free range is added.
    ///
    /// # Arguments
    ///
    /// * `new_end` - The new end point for the range (must be greater than the current end).
    pub fn grow_to(&mut self, new_end: T) {
        let initial_range_end = self.initial_range.end;
        if let Some(last_range) = self
            .free_ranges
            .last_mut()
            .filter(|last_range| last_range.end == initial_range_end)
        {
            last_range.end = new_end;
        } else {
            self.free_ranges.push(self.initial_range.end..new_end);
        }

        self.initial_range.end = new_end;
    }

    /// Allocates a contiguous range of the specified length.
    ///
    /// This method uses a best-fit allocation strategy to find the smallest free range
    /// that can satisfy the request, minimizing fragmentation. If no single contiguous
    /// range is large enough, it returns an error with information about the total
    /// fragmented free space.
    ///
    /// # Arguments
    ///
    /// * `length` - The length of the range to allocate.
    ///
    /// # Returns
    ///
    /// * `Ok(Range<T>)` - The allocated range if successful.
    /// * `Err(RangeAllocationError<T>)` - If allocation fails, containing information
    ///   about the total fragmented free space available.
    ///
    /// # Example
    ///
    /// ```
    /// use range_alloc_arceos::RangeAllocator;
    ///
    /// let mut allocator = RangeAllocator::new(0..100);
    /// let range = allocator.allocate_range(20).unwrap();
    /// assert_eq!(range, 0..20);
    /// ```
    pub fn allocate_range(&mut self, length: T) -> Result<Range<T>, RangeAllocationError<T>> {
        assert_ne!(length + length, length);
        let mut best_fit: Option<(usize, Range<T>)> = None;

        // This is actually correct. With the trait bound as it is, we have
        // no way to summon a value of 0 directly, so we make one by subtracting
        // something from itself. Once the trait bound can be changed, this can
        // be fixed.
        #[allow(clippy::eq_op)]
        let mut fragmented_free_length = length - length;
        for (index, range) in self.free_ranges.iter().cloned().enumerate() {
            let range_length = range.end - range.start;
            fragmented_free_length += range_length;
            if range_length < length {
                continue;
            } else if range_length == length {
                // Found a perfect fit, so stop looking.
                best_fit = Some((index, range));
                break;
            }
            best_fit = Some(match best_fit {
                Some((best_index, best_range)) => {
                    // Find best fit for this allocation to reduce memory fragmentation.
                    if range_length < best_range.end - best_range.start {
                        (index, range)
                    } else {
                        (best_index, best_range.clone())
                    }
                }
                None => (index, range),
            });
        }
        match best_fit {
            Some((index, range)) => {
                if range.end - range.start == length {
                    self.free_ranges.remove(index);
                } else {
                    self.free_ranges[index].start += length;
                }
                Ok(range.start..(range.start + length))
            }
            None => Err(RangeAllocationError {
                fragmented_free_length,
            }),
        }
    }

    /// Frees a previously allocated range, making it available for future allocations.
    ///
    /// This method attempts to merge the freed range with adjacent free ranges to
    /// reduce fragmentation. The freed range must be within the initial range and
    /// must not be empty.
    ///
    /// # Arguments
    ///
    /// * `range` - The range to free. Must be within the allocator's initial range.
    ///
    /// # Panics
    ///
    /// Panics if the range is outside the initial range or if the range is empty
    /// (start >= end).
    ///
    /// # Example
    ///
    /// ```
    /// use range_alloc_arceos::RangeAllocator;
    ///
    /// let mut allocator = RangeAllocator::new(0..100);
    /// let range = allocator.allocate_range(20).unwrap();
    /// allocator.free_range(range);
    /// ```
    pub fn free_range(&mut self, range: Range<T>) {
        assert!(self.initial_range.start <= range.start && range.end <= self.initial_range.end);
        assert!(range.start < range.end);

        // Get insertion position.
        let i = self
            .free_ranges
            .iter()
            .position(|r| r.start > range.start)
            .unwrap_or(self.free_ranges.len());

        // Try merging with neighboring ranges in the free list.
        // Before: |left|-(range)-|right|
        if i > 0 && range.start == self.free_ranges[i - 1].end {
            // Merge with |left|.
            self.free_ranges[i - 1].end =
                if i < self.free_ranges.len() && range.end == self.free_ranges[i].start {
                    // Check for possible merge with |left| and |right|.
                    let right = self.free_ranges.remove(i);
                    right.end
                } else {
                    range.end
                };

            return;
        } else if i < self.free_ranges.len() && range.end == self.free_ranges[i].start {
            // Merge with |right|.
            self.free_ranges[i].start = if i > 0 && range.start == self.free_ranges[i - 1].end {
                // Check for possible merge with |left| and |right|.
                let left = self.free_ranges.remove(i - 1);
                left.start
            } else {
                range.start
            };

            return;
        }

        // Debug checks
        assert!(
            (i == 0 || self.free_ranges[i - 1].end < range.start)
                && (i >= self.free_ranges.len() || range.end < self.free_ranges[i].start)
        );

        self.free_ranges.insert(i, range);
    }

    /// Returns an iterator over allocated non-empty ranges
    pub fn allocated_ranges(&self) -> impl Iterator<Item = Range<T>> + '_ {
        let first = match self.free_ranges.first() {
            Some(Range { ref start, .. }) if *start > self.initial_range.start => {
                Some(self.initial_range.start..*start)
            }
            None => Some(self.initial_range.clone()),
            _ => None,
        };

        let last = match self.free_ranges.last() {
            Some(Range { end, .. }) if *end < self.initial_range.end => {
                Some(*end..self.initial_range.end)
            }
            _ => None,
        };

        let mid = self
            .free_ranges
            .iter()
            .zip(self.free_ranges.iter().skip(1))
            .map(|(ra, rb)| ra.end..rb.start);

        first.into_iter().chain(mid).chain(last)
    }

    /// Resets the allocator to its initial state.
    ///
    /// This marks the entire initial range as free, effectively deallocating
    /// all previously allocated ranges.
    pub fn reset(&mut self) {
        self.free_ranges.clear();
        self.free_ranges.push(self.initial_range.clone());
    }

    /// Returns `true` if no ranges have been allocated.
    ///
    /// This checks whether the allocator is in its initial state with all space free.
    pub fn is_empty(&self) -> bool {
        self.free_ranges.len() == 1 && self.free_ranges[0] == self.initial_range
    }
}

impl<T: Copy + Sub<Output = T> + Sum> RangeAllocator<T> {
    /// Returns the total amount of free space available across all free ranges.
    ///
    /// This sums the lengths of all free ranges, giving the total amount of space
    /// that could be allocated if fragmentation is not an issue.
    pub fn total_available(&self) -> T {
        self.free_ranges
            .iter()
            .map(|range| range.end - range.start)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_allocation() {
        let mut alloc = RangeAllocator::new(0..10);
        // Test if an allocation works
        assert_eq!(alloc.allocate_range(4), Ok(0..4));
        assert!(alloc.allocated_ranges().eq(core::iter::once(0..4)));
        // Free the prior allocation
        alloc.free_range(0..4);
        // Make sure the free actually worked
        assert_eq!(alloc.free_ranges, vec![0..10]);
        assert!(alloc.allocated_ranges().eq(core::iter::empty()));
    }

    #[test]
    fn test_out_of_space() {
        let mut alloc = RangeAllocator::new(0..10);
        // Test if the allocator runs out of space correctly
        assert_eq!(alloc.allocate_range(10), Ok(0..10));
        assert!(alloc.allocated_ranges().eq(core::iter::once(0..10)));
        assert!(alloc.allocate_range(4).is_err());
        alloc.free_range(0..10);
    }

    #[test]
    fn test_grow() {
        let mut alloc = RangeAllocator::new(0..11);
        // Test if the allocator runs out of space correctly
        assert_eq!(alloc.allocate_range(10), Ok(0..10));
        assert!(alloc.allocated_ranges().eq(core::iter::once(0..10)));
        assert!(alloc.allocate_range(4).is_err());
        alloc.grow_to(20);
        assert_eq!(alloc.allocate_range(4), Ok(10..14));
        alloc.free_range(0..14);
    }

    #[test]
    fn test_grow_with_hole_at_start() {
        let mut alloc = RangeAllocator::new(0..6);

        assert_eq!(alloc.allocate_range(3), Ok(0..3));
        assert_eq!(alloc.allocate_range(3), Ok(3..6));
        alloc.free_range(0..3);

        alloc.grow_to(9);
        assert_eq!(alloc.allocated_ranges().collect::<Vec<_>>(), [3..6]);
    }
    #[test]
    fn test_grow_with_hole_in_middle() {
        let mut alloc = RangeAllocator::new(0..6);

        assert_eq!(alloc.allocate_range(2), Ok(0..2));
        assert_eq!(alloc.allocate_range(2), Ok(2..4));
        assert_eq!(alloc.allocate_range(2), Ok(4..6));
        alloc.free_range(2..4);

        alloc.grow_to(9);
        assert_eq!(alloc.allocated_ranges().collect::<Vec<_>>(), [0..2, 4..6]);
    }

    #[test]
    fn test_dont_use_block_that_is_too_small() {
        let mut alloc = RangeAllocator::new(0..10);
        // Allocate three blocks then free the middle one and check for correct state
        assert_eq!(alloc.allocate_range(3), Ok(0..3));
        assert_eq!(alloc.allocate_range(3), Ok(3..6));
        assert_eq!(alloc.allocate_range(3), Ok(6..9));
        alloc.free_range(3..6);
        assert_eq!(alloc.free_ranges, vec![3..6, 9..10]);
        assert_eq!(
            alloc.allocated_ranges().collect::<Vec<Range<i32>>>(),
            vec![0..3, 6..9]
        );
        // Now request space that the middle block can fill, but the end one can't.
        assert_eq!(alloc.allocate_range(3), Ok(3..6));
    }

    #[test]
    fn test_free_blocks_in_middle() {
        let mut alloc = RangeAllocator::new(0..100);
        // Allocate many blocks then free every other block.
        assert_eq!(alloc.allocate_range(10), Ok(0..10));
        assert_eq!(alloc.allocate_range(10), Ok(10..20));
        assert_eq!(alloc.allocate_range(10), Ok(20..30));
        assert_eq!(alloc.allocate_range(10), Ok(30..40));
        assert_eq!(alloc.allocate_range(10), Ok(40..50));
        assert_eq!(alloc.allocate_range(10), Ok(50..60));
        assert_eq!(alloc.allocate_range(10), Ok(60..70));
        assert_eq!(alloc.allocate_range(10), Ok(70..80));
        assert_eq!(alloc.allocate_range(10), Ok(80..90));
        assert_eq!(alloc.allocate_range(10), Ok(90..100));
        assert_eq!(alloc.free_ranges, vec![]);
        assert!(alloc.allocated_ranges().eq(core::iter::once(0..100)));
        alloc.free_range(10..20);
        alloc.free_range(30..40);
        alloc.free_range(50..60);
        alloc.free_range(70..80);
        alloc.free_range(90..100);
        // Check that the right blocks were freed.
        assert_eq!(
            alloc.free_ranges,
            vec![10..20, 30..40, 50..60, 70..80, 90..100]
        );
        assert_eq!(
            alloc.allocated_ranges().collect::<Vec<Range<i32>>>(),
            vec![0..10, 20..30, 40..50, 60..70, 80..90]
        );
        // Fragment the memory on purpose a bit.
        assert_eq!(alloc.allocate_range(6), Ok(10..16));
        assert_eq!(alloc.allocate_range(6), Ok(30..36));
        assert_eq!(alloc.allocate_range(6), Ok(50..56));
        assert_eq!(alloc.allocate_range(6), Ok(70..76));
        assert_eq!(alloc.allocate_range(6), Ok(90..96));
        // Check for fragmentation.
        assert_eq!(
            alloc.free_ranges,
            vec![16..20, 36..40, 56..60, 76..80, 96..100]
        );
        assert_eq!(
            alloc.allocated_ranges().collect::<Vec<Range<i32>>>(),
            vec![0..16, 20..36, 40..56, 60..76, 80..96]
        );
        // Fill up the fragmentation
        assert_eq!(alloc.allocate_range(4), Ok(16..20));
        assert_eq!(alloc.allocate_range(4), Ok(36..40));
        assert_eq!(alloc.allocate_range(4), Ok(56..60));
        assert_eq!(alloc.allocate_range(4), Ok(76..80));
        assert_eq!(alloc.allocate_range(4), Ok(96..100));
        // Check that nothing is free.
        assert_eq!(alloc.free_ranges, vec![]);
        assert!(alloc.allocated_ranges().eq(core::iter::once(0..100)));
    }

    #[test]
    fn test_ignore_block_if_another_fits_better() {
        let mut alloc = RangeAllocator::new(0..10);
        // Allocate blocks such that the only free spaces available are 3..6 and 9..10
        // in order to prepare for the next test.
        assert_eq!(alloc.allocate_range(3), Ok(0..3));
        assert_eq!(alloc.allocate_range(3), Ok(3..6));
        assert_eq!(alloc.allocate_range(3), Ok(6..9));
        alloc.free_range(3..6);
        assert_eq!(alloc.free_ranges, vec![3..6, 9..10]);
        assert_eq!(
            alloc.allocated_ranges().collect::<Vec<Range<i32>>>(),
            vec![0..3, 6..9]
        );
        // Now request space that can be filled by 3..6 but should be filled by 9..10
        // because 9..10 is a perfect fit.
        assert_eq!(alloc.allocate_range(1), Ok(9..10));
    }

    #[test]
    fn test_merge_neighbors() {
        let mut alloc = RangeAllocator::new(0..9);
        assert_eq!(alloc.allocate_range(3), Ok(0..3));
        assert_eq!(alloc.allocate_range(3), Ok(3..6));
        assert_eq!(alloc.allocate_range(3), Ok(6..9));
        alloc.free_range(0..3);
        alloc.free_range(6..9);
        alloc.free_range(3..6);
        assert_eq!(alloc.free_ranges, vec![0..9]);
        assert!(alloc.allocated_ranges().eq(core::iter::empty()));
    }
}
