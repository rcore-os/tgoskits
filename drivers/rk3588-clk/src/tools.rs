// Allow Clippy warnings for math operations
#![allow(clippy::manual_div_ceil)]

pub fn div_to_rate(rate: usize, div: u32) -> usize {
    rate / (div as usize + 1)
}

pub fn div_round_up(dividend: usize, divisor: usize) -> usize {
    (dividend + divisor - 1) / divisor
}
