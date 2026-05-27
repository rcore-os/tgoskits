// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

/// Find the last (most significant) bit set in a 32-bit value.
///
/// Bits are numbered starting at 0 (the least significant bit).
/// A return value of `INVALID_BIT_INDEX` indicates that the input value was zero,
/// and no bits are set.
///
/// # Parameters
/// - `value`: A `u32` input value.
///
/// # Returns
/// - Zero-based bit index of the most significant bit set, or `INVALID_BIT_INDEX` if `value` is zero.
pub fn fls32(value: u32) -> u16 {
    const INVALID_BIT_INDEX: u16 = 0xFFFF; // Define invalid bit index for zero input
    if value == 0 {
        return INVALID_BIT_INDEX;
    }
    31 - value.leading_zeros() as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVALID_BIT_INDEX: u16 = 0xFFFF;

    #[test]
    fn test_fls32() {
        // Test case: input is 0, no bits set
        assert_eq!(fls32(0x0), INVALID_BIT_INDEX);

        // Test case: input is 1 (0b00000001), bit 0 is set
        assert_eq!(fls32(0x01), 0);

        // Test case: input is 128 (0b10000000), bit 7 is set
        assert_eq!(fls32(0x80), 7);

        // Test case: input is 0x80000001, bit 31 is the most significant bit set
        assert_eq!(fls32(0x80000001), 31);

        // Test case: input is 0xFFFFFFFF, bit 31 is the most significant bit set
        assert_eq!(fls32(0xFFFFFFFF), 31);

        // Test case: input is 0x7FFFFFFF, bit 30 is the most significant bit set
        assert_eq!(fls32(0x7FFFFFFF), 30);
    }

    #[test]
    fn test_fls32_edge_cases() {
        // Test case: input is 0x00000010, bit 4 is set
        assert_eq!(fls32(0x10), 4);

        // Test case: input is 0x00001000, bit 12 is set
        assert_eq!(fls32(0x1000), 12);

        // Test case: input is the maximum value (0xFFFFFFFF), bit 31 is set
        assert_eq!(fls32(u32::MAX), 31);

        // Test case: input is 0x8000_0000 (highest bit set), bit 31 is set
        assert_eq!(fls32(0x8000_0000), 31);
    }
}
