//! Lowest-first range search shared by address and MSI allocators.

use core::ops::Range;

use crate::resources::pool::{RangeOwner, ranges_overlap};

pub(super) fn find_u64_range(
    pools: &[Range<u64>],
    occupied: &[RangeOwner<u64>],
    size: u64,
    alignment: u64,
) -> Option<u64> {
    for pool in pools {
        let mut candidate = align_up(pool.start, alignment)?;
        while candidate.checked_add(size)? <= pool.end {
            let end = candidate + size;
            let conflict = first_conflict(occupied, candidate, end);
            match conflict {
                Some(owner) => candidate = align_up(owner.range.end, alignment)?,
                None => return Some(candidate),
            }
        }
    }
    None
}

pub(super) fn find_u16_range(
    pools: &[Range<u16>],
    occupied: &[RangeOwner<u16>],
    size: u16,
    alignment: u16,
) -> Option<u16> {
    for pool in pools {
        let mut candidate = align_up(u64::from(pool.start), u64::from(alignment))?;
        while candidate.checked_add(u64::from(size))? <= u64::from(pool.end) {
            let end = candidate + u64::from(size);
            let conflict = occupied
                .iter()
                .filter(|owner| {
                    ranges_overlap(
                        candidate,
                        end,
                        u64::from(owner.range.start),
                        u64::from(owner.range.end),
                    )
                })
                .min_by_key(|owner| owner.range.start);
            match conflict {
                Some(owner) => {
                    candidate = align_up(u64::from(owner.range.end), u64::from(alignment))?
                }
                None => return u16::try_from(candidate).ok(),
            }
        }
    }
    None
}

pub(super) fn find_u32_range(
    pools: &[Range<u32>],
    occupied: &[RangeOwner<u32>],
    size: u32,
) -> Option<u32> {
    for pool in pools {
        let mut candidate = pool.start;
        while candidate.checked_add(size)? <= pool.end {
            let end = candidate + size;
            let conflict = first_conflict(occupied, candidate, end);
            match conflict {
                Some(owner) => candidate = owner.range.end,
                None => return Some(candidate),
            }
        }
    }
    None
}

pub(super) fn range_allowed<T>(pools: Option<&[Range<T>]>, start: T, size: T) -> bool
where
    T: Copy + Ord + CheckedAdd,
{
    let Some(end) = start.checked_add(size) else {
        return false;
    };
    pools.is_some_and(|ranges| {
        ranges
            .iter()
            .any(|pool| start >= pool.start && end <= pool.end)
    })
}

fn first_conflict<T: Copy + Ord>(
    occupied: &[RangeOwner<T>],
    start: T,
    end: T,
) -> Option<&RangeOwner<T>> {
    occupied
        .iter()
        .filter(|owner| ranges_overlap(start, end, owner.range.start, owner.range.end))
        .min_by_key(|owner| owner.range.start)
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    let mask = alignment.checked_sub(1)?;
    value.checked_add(mask).map(|value| value & !mask)
}

pub(super) trait CheckedAdd: Sized {
    fn checked_add(self, other: Self) -> Option<Self>;
}

impl CheckedAdd for u16 {
    fn checked_add(self, other: Self) -> Option<Self> {
        u16::checked_add(self, other)
    }
}

impl CheckedAdd for u32 {
    fn checked_add(self, other: Self) -> Option<Self> {
        u32::checked_add(self, other)
    }
}

impl CheckedAdd for u64 {
    fn checked_add(self, other: Self) -> Option<Self> {
        u64::checked_add(self, other)
    }
}
