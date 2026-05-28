use alloc::{vec, vec::Vec};
use core::ops::Range;

/// A minimal best-fit range allocator for IVC GPA ranges.
#[derive(Debug)]
pub(crate) struct RangeAllocator {
    initial: Range<usize>,
    free: Vec<Range<usize>>,
}

impl RangeAllocator {
    pub(crate) fn new(range: Range<usize>) -> Self {
        Self {
            initial: range.clone(),
            free: vec![range],
        }
    }

    pub(crate) fn allocate_range(&mut self, size: usize) -> Option<Range<usize>> {
        debug_assert!(size > 0);

        let mut best_fit = None;
        for (index, range) in self.free.iter().enumerate() {
            let len = range.end - range.start;
            if len < size {
                continue;
            }
            if len == size {
                best_fit = Some(index);
                break;
            }
            match best_fit {
                Some(best_index)
                    if len >= self.free[best_index].end - self.free[best_index].start => {}
                _ => best_fit = Some(index),
            }
        }

        let index = best_fit?;
        let start = self.free[index].start;
        let end = start + size;
        if self.free[index].end == end {
            self.free.remove(index);
        } else {
            self.free[index].start = end;
        }
        Some(start..end)
    }

    pub(crate) fn free_range(&mut self, range: Range<usize>) -> bool {
        if range.start >= range.end
            || range.start < self.initial.start
            || range.end > self.initial.end
        {
            return false;
        }

        let index = self
            .free
            .iter()
            .position(|free| free.start > range.start)
            .unwrap_or(self.free.len());

        if index > 0 && self.free[index - 1].end > range.start {
            return false;
        }
        if index < self.free.len() && range.end > self.free[index].start {
            return false;
        }

        if index > 0 && self.free[index - 1].end == range.start {
            self.free[index - 1].end = range.end;
            if index < self.free.len() && self.free[index - 1].end == self.free[index].start {
                let next = self.free.remove(index);
                self.free[index - 1].end = next.end;
            }
        } else if index < self.free.len() && range.end == self.free[index].start {
            self.free[index].start = range.start;
        } else {
            self.free.insert(index, range);
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::RangeAllocator;

    #[test]
    fn allocates_and_reuses_ranges() {
        let mut allocator = RangeAllocator::new(0..0x4000);

        assert_eq!(allocator.allocate_range(0x1000), Some(0..0x1000));
        assert_eq!(allocator.allocate_range(0x1000), Some(0x1000..0x2000));

        assert!(allocator.free_range(0..0x1000));
        assert_eq!(allocator.allocate_range(0x1000), Some(0..0x1000));
    }

    #[test]
    fn picks_best_fit_range() {
        let mut allocator = RangeAllocator::new(0..0x9000);

        assert_eq!(allocator.allocate_range(0x3000), Some(0..0x3000));
        assert_eq!(allocator.allocate_range(0x3000), Some(0x3000..0x6000));
        assert_eq!(allocator.allocate_range(0x3000), Some(0x6000..0x9000));

        assert!(allocator.free_range(0..0x3000));
        assert!(allocator.free_range(0x6000..0x9000));

        assert_eq!(allocator.allocate_range(0x3000), Some(0..0x3000));
        assert_eq!(allocator.allocate_range(0x3000), Some(0x6000..0x9000));
    }

    #[test]
    fn merges_neighboring_freed_ranges() {
        let mut allocator = RangeAllocator::new(0..0x3000);

        assert_eq!(allocator.allocate_range(0x1000), Some(0..0x1000));
        assert_eq!(allocator.allocate_range(0x1000), Some(0x1000..0x2000));
        assert_eq!(allocator.allocate_range(0x1000), Some(0x2000..0x3000));

        assert!(allocator.free_range(0..0x1000));
        assert!(allocator.free_range(0x2000..0x3000));
        assert!(allocator.free_range(0x1000..0x2000));

        assert_eq!(allocator.allocate_range(0x3000), Some(0..0x3000));
    }

    #[test]
    fn rejects_invalid_or_duplicate_frees() {
        let mut allocator = RangeAllocator::new(0x1000..0x3000);

        assert!(!allocator.free_range(0x1000..0x1000));
        assert!(!allocator.free_range(0..0x1000));
        assert!(!allocator.free_range(0x2000..0x4000));
        assert!(!allocator.free_range(0x1000..0x2000));

        assert_eq!(allocator.allocate_range(0x1000), Some(0x1000..0x2000));
        assert!(allocator.free_range(0x1000..0x2000));
        assert!(!allocator.free_range(0x1000..0x2000));
    }
}
