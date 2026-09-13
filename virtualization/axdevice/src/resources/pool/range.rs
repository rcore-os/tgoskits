//! Range validation and owner-aware reservations shared by pool namespaces.

use alloc::{format, string::String, vec::Vec};
use core::ops::Range;

use super::RangeOwner;
use crate::{DeviceManagerError, DeviceManagerResult};

pub(super) fn insert_range<T>(
    ranges: &mut Vec<Range<T>>,
    range: Range<T>,
    kind: &'static str,
) -> DeviceManagerResult
where
    T: Copy + Ord + core::fmt::LowerHex,
{
    validate_range(&range, kind)?;
    if !ranges
        .iter()
        .any(|existing| existing.start == range.start && existing.end == range.end)
    {
        ranges.push(range);
        ranges.sort_by_key(|candidate| candidate.start);
    }
    Ok(())
}

pub(super) fn reserve_range<T>(
    reservations: &mut Vec<RangeOwner<T>>,
    owner: String,
    range: Range<T>,
    kind: &'static str,
) -> DeviceManagerResult
where
    T: Copy + Ord + core::fmt::LowerHex,
{
    validate_range(&range, kind)?;
    if let Some(existing) = reservations.iter().find(|existing| {
        ranges_overlap(
            range.start,
            range.end,
            existing.range.start,
            existing.range.end,
        )
    }) {
        return Err(DeviceManagerError::ResourceConflict {
            operation: "reserve device resource",
            detail: format!(
                "{kind} range {:#x}..{:#x} for {owner} overlaps reservation owned by {}",
                range.start, range.end, existing.owner
            ),
        });
    }
    reservations.push(RangeOwner { range, owner });
    Ok(())
}

pub(super) fn nonempty_owner(owner: String) -> DeviceManagerResult<String> {
    if owner.is_empty() {
        return Err(DeviceManagerError::InvalidInput {
            operation: "reserve device resource",
            detail: "resource owner must not be empty".into(),
        });
    }
    Ok(owner)
}

fn validate_range<T>(range: &Range<T>, kind: &'static str) -> DeviceManagerResult
where
    T: Copy + Ord + core::fmt::LowerHex,
{
    if range.start >= range.end {
        return Err(DeviceManagerError::InvalidConfig {
            operation: "configure resource pool",
            detail: format!("{kind} range {:#x}..{:#x} is empty", range.start, range.end),
        });
    }
    Ok(())
}

pub(crate) fn ranges_overlap<T: Ord>(start: T, end: T, other_start: T, other_end: T) -> bool {
    start < other_end && other_start < end
}
