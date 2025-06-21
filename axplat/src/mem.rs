//! Physical memory information.

use core::{fmt, ops::Range};

use memory_addr::{PhysAddr, VirtAddr};

bitflags::bitflags! {
    /// The flags of a physical memory region.
    #[derive(Clone, Copy)]
    pub struct MemRegionFlags: usize {
        /// Readable.
        const READ          = 1 << 0;
        /// Writable.
        const WRITE         = 1 << 1;
        /// Executable.
        const EXECUTE       = 1 << 2;
        /// Device memory. (e.g., MMIO regions)
        const DEVICE        = 1 << 4;
        /// Uncachable memory. (e.g., framebuffer)
        const UNCACHED      = 1 << 5;
        /// Reserved memory, do not use for allocation.
        const RESERVED      = 1 << 6;
        /// Free memory for allocation.
        const FREE          = 1 << 7;
    }
}

impl fmt::Debug for MemRegionFlags {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

/// The default flags for a normal memory region (readable, writable and allocatable).
pub const DEFAULT_RAM_FLAGS: MemRegionFlags = MemRegionFlags::READ
    .union(MemRegionFlags::WRITE)
    .union(MemRegionFlags::FREE);

/// The default flags for a reserved memory region (readable, writable, and reserved).
pub const DEFAULT_RESERVED_FLAGS: MemRegionFlags = MemRegionFlags::READ
    .union(MemRegionFlags::WRITE)
    .union(MemRegionFlags::RESERVED);

/// The default flags for a MMIO region (readable, writable, device, and reserved).
pub const DEFAULT_MMIO_FLAGS: MemRegionFlags = MemRegionFlags::READ
    .union(MemRegionFlags::WRITE)
    .union(MemRegionFlags::DEVICE)
    .union(MemRegionFlags::RESERVED);

/// The raw memory range with start and size.
pub type RawRange = (usize, usize);

/// A physical memory region.
#[derive(Debug, Clone, Copy)]
pub struct PhysMemRegion {
    /// The start physical address of the region.
    pub paddr: PhysAddr,
    /// The size in bytes of the region.
    pub size: usize,
    /// The region flags, see [`MemRegionFlags`].
    pub flags: MemRegionFlags,
    /// The region name, used for identification.
    pub name: &'static str,
}

impl PhysMemRegion {
    /// Creates a RAM region with default flags (readable, writable, and allocatable).
    pub const fn new_ram(start: usize, size: usize, name: &'static str) -> Self {
        Self {
            paddr: PhysAddr::from_usize(start),
            size,
            flags: DEFAULT_RAM_FLAGS,
            name,
        }
    }

    /// Creates a MMIO region with default flags (readable, writable, and device).
    pub const fn new_mmio(start: usize, size: usize, name: &'static str) -> Self {
        Self {
            paddr: PhysAddr::from_usize(start),
            size,
            flags: DEFAULT_MMIO_FLAGS,
            name,
        }
    }

    /// Creates a reserved memory region with default flags (readable, writable, and reserved).
    pub const fn new_reserved(start: usize, size: usize, name: &'static str) -> Self {
        Self {
            paddr: PhysAddr::from_usize(start),
            size,
            flags: DEFAULT_RESERVED_FLAGS,
            name,
        }
    }
}

/// Physical memory interface.
#[def_plat_interface]
pub trait MemIf {
    /// Returns all physical memory (RAM) ranges on the platform.
    ///
    /// All memory ranges except reserved ranges (including the kernel loaded
    /// range) are free for allocation.
    fn phys_ram_ranges() -> &'static [RawRange];

    /// Returns all reserved physical memory ranges on the platform.
    ///
    /// Reserved memory can be contained in [`phys_ram_ranges`], they are not
    /// allocatable but should be mapped to kernel's address space.
    ///
    /// Note that the ranges returned should not include the range where the
    /// kernel is loaded.
    fn reserved_phys_ram_ranges() -> &'static [RawRange];

    /// Returns all device memory (MMIO) ranges on the platform.
    fn mmio_ranges() -> &'static [RawRange];

    /// Translates a physical address to a virtual address.
    ///
    /// It is just an easy way to access physical memory when virtual memory
    /// is enabled. The mapping may not be unique, there can be multiple `vaddr`s
    /// mapped to that `paddr`.
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr;

    /// Translates a virtual address to a physical address.
    ///
    /// It is a reverse operation of [`phys_to_virt`]. It requires that the
    /// `vaddr` must be available through the [`phys_to_virt`] translation.
    /// It **cannot** be used to translate arbitrary virtual addresses.
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr;
}

/// Returns the total size of physical memory (RAM) on the platform.
///
/// It should be equal to the sum of sizes of all physical memory ranges (returned
/// by [`phys_ram_ranges`]).
pub fn total_ram_size() -> usize {
    phys_ram_ranges().iter().map(|range| range.1).sum()
}

/// The error type for overlapping check.
///
/// It contains the overlapping range pair.
pub type OverlapErr = (Range<usize>, Range<usize>);

/// Checks if the given ranges are overlapping.
///
/// Returns `Err` with one of the overlapping range pair if they are overlapping.
///
/// The given ranges should be sorted by the start, otherwise it always returns
/// `Err`.
///
/// # Example
///
/// ```rust
/// # use axplat::mem::check_sorted_ranges_overlap;
/// assert!(check_sorted_ranges_overlap([(0, 10), (10, 10)].into_iter()).is_ok());
/// assert_eq!(
///     check_sorted_ranges_overlap([(0, 10), (5, 10)].into_iter()),
///     Err((0..10, 5..15))
/// );
/// ```
pub fn check_sorted_ranges_overlap(
    ranges: impl Iterator<Item = RawRange>,
) -> Result<(), OverlapErr> {
    let mut prev = Range::default();
    for (start, size) in ranges {
        if prev.end > start {
            return Err((prev, start..start + size));
        }
        prev = start..start + size;
    }
    Ok(())
}

/// Removes a portion of ranges from the given ranges.
///
/// `from` is a list of ranges to be operated on, and `exclude` is a list of
/// ranges to be removed. `exclude` should have been sorted by the start, and
/// have non-overlapping ranges. If not, an error will be returned.
///
/// The result is also a list of ranges with each range contained in `from` but
/// not in `exclude`. `result_op` is a closure that will be called for each range
/// in the result.
///
/// # Example
///
/// ```rust
/// # use axplat::mem::ranges_difference;
/// let mut res = Vec::new();
/// // 0..10, 20..30 - 5..15, 15..25 = 0..5, 25..30
/// ranges_difference(&[(0, 10), (20, 10)], &[(5, 10), (15, 10)], |r| res.push(r)).unwrap();
/// assert_eq!(res, &[(0, 5), (25, 5)]);
/// ```
pub fn ranges_difference<F>(
    from: &[RawRange],
    exclude: &[RawRange],
    mut result_op: F,
) -> Result<(), OverlapErr>
where
    F: FnMut(RawRange),
{
    check_sorted_ranges_overlap(from.iter().cloned())?;

    for &(start, size) in from {
        let mut start = start;
        let end = start + size;

        for &(exclude_start, exclude_size) in exclude {
            let exclude_end = exclude_start + exclude_size;
            if exclude_end <= start {
                continue;
            } else if exclude_start >= end {
                break;
            } else if exclude_start > start {
                result_op((start, exclude_start - start));
            }
            start = exclude_end;
        }
        if start < end {
            result_op((start, end - start));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_sorted_ranges_overlap() {
        use super::check_sorted_ranges_overlap as f;

        assert!(f([(0, 10), (10, 10), (20, 10)].into_iter()).is_ok());
        assert!(f([(0, 10), (20, 10), (40, 10)].into_iter()).is_ok());
        assert_eq!(f([(0, 1), (0, 2)].into_iter()), Err((0..1, 0..2)));
        assert_eq!(
            f([(0, 11), (10, 10), (20, 10)].into_iter()),
            Err((0..11, 10..20)),
        );
        assert_eq!(
            f([(0, 10), (20, 10), (10, 10)].into_iter()),
            Err((20..30, 10..20)), // not sorted
        );
    }

    #[test]
    fn ranges_difference() {
        let f = |from, exclude| {
            let mut res = Vec::new();
            super::ranges_difference(from, exclude, |r| res.push(r)).unwrap();
            res
        };

        // 0..10, 20..30
        assert_eq!(
            f(&[(0, 10), (20, 10)], &[(5, 5), (25, 5)]), // - 5..10, 25..30
            &[(0, 5), (20, 5)]                           // = 0..5, 20..25
        );
        assert_eq!(
            f(&[(0, 10), (20, 10)], &[(5, 10), (15, 5)]), // - 5..15, 15..20
            &[(0, 5), (20, 10)]                           // = 0..5, 20..30
        );
        assert_eq!(
            f(&[(0, 10), (20, 10)], &[(5, 1), (25, 1), (30, 1)]), // - 5..6, 25..26, 30..31
            &[(0, 5), (6, 4), (20, 5), (26, 4)]                   // = 0..5, 6..10, 20..25, 26..30
        );

        // 0..10, 20..30
        assert_eq!(f(&[(0, 10), (20, 10)], &[(5, 20)]), &[(0, 5), (25, 5)]); // - 5..25 = 0..5, 25..30
        assert_eq!(f(&[(0, 10), (20, 10)], &[(0, 30)]), &[]); // - 0..30 = []

        // 0..30
        assert_eq!(
            f(&[(0, 30)], &[(0, 5), (10, 5), (20, 5)]), // - 0..5, 10..15, 20..25
            &[(5, 5), (15, 5), (25, 5)]                 // = 5..10, 15..20, 25..30
        );
        assert_eq!(
            f(
                &[(0, 30)],
                &[(0, 5), (5, 5), (10, 5), (15, 5), (20, 5), (25, 5)] // - 0..5, 5..10, 10..15, 15..20, 20..25, 25..30
            ),
            &[] // = []
        );

        // 10..20
        assert_eq!(f(&[(10, 10)], &[(0, 30)]), &[]); // - 0..30 = []
    }
}
