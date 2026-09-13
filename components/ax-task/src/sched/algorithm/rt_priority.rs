//! Linux real-time priority-array numbering.

/// Linux reserves `MAX_RT_PRIO` (100) for the first non-RT priority, leaving
/// internal RT priorities `0..MAX_RT_PRIO - 1`. POSIX priority 99 therefore
/// maps to internal priority 0, while POSIX priority 1 maps to internal 98.
pub(crate) const RT_PRIORITY_LEVELS: usize = 99;

/// Converts a validated POSIX RT priority to Linux's internal priority-array
/// index. The caller must provide a priority in `1..=99`.
pub(crate) const fn rt_priority_index(priority: u8) -> usize {
    assert!(priority != 0 && priority <= RT_PRIORITY_LEVELS as u8);
    (RT_PRIORITY_LEVELS as u8 - priority) as usize
}

/// Converts a Linux internal RT priority-array index back to POSIX numbering.
pub(crate) const fn rt_priority_from_index(index: usize) -> u8 {
    assert!(index < RT_PRIORITY_LEVELS);
    RT_PRIORITY_LEVELS as u8 - index as u8
}

/// Selects the highest POSIX RT priority represented by a Linux-style bitmap.
pub(crate) fn bitmap_highest_rt_priority(bitmap: u128) -> Option<u8> {
    let index = bitmap.trailing_zeros() as usize;
    (index < RT_PRIORITY_LEVELS).then(|| rt_priority_from_index(index))
}
