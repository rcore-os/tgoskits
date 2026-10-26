// SPDX-License-Identifier: MIT
//! Short sleeps for device polling loops.

/// Sleeps for `min_us` microseconds. The upper bound `_max_us` is accepted for
/// call-site compatibility; the timer resolution here is finer than any slack
/// a caller would ask for, so the lower bound is what is honoured.
pub fn usleep_range(min_us: u64, _max_us: u64) {
    if min_us != 0 {
        crate::clock::sleep_us(min_us);
    }
}
