//! Shared bitmap utility helpers.

/// Returns the number of bytes required to store `bits` bits.
pub fn bytes_for_bits(bits: u32) -> usize {
    bits.div_ceil(8) as usize
}

/// Returns the number of set bits in a byte.
pub fn count_set_bits(byte: u8) -> u32 {
    byte.count_ones()
}

/// Counts the number of set bits in a bitmap up to `max_bits`.
pub fn count_set_bits_in_bitmap(data: &[u8], max_bits: u32) -> u32 {
    let mut count = 0u32;
    let full_bytes = (max_bits / 8) as usize;
    let remaining_bits = (max_bits % 8) as u8;

    for &byte in &data[..full_bytes.min(data.len())] {
        count += count_set_bits(byte);
    }

    if full_bytes < data.len() && remaining_bits > 0 {
        let mask = (1u8 << remaining_bits) - 1;
        count += count_set_bits(data[full_bytes] & mask);
    }

    count
}

/// Sets a bit in-place. Returns `false` if the bit is out of range.
pub fn set_bit(data: &mut [u8], bit_idx: u32) -> bool {
    let byte_idx = (bit_idx / 8) as usize;
    let bit_pos = (bit_idx % 8) as u8;

    if byte_idx >= data.len() {
        return false;
    }

    data[byte_idx] |= 1 << bit_pos;
    true
}

/// Clears a bit in-place. Returns `false` if the bit is out of range.
pub fn clear_bit(data: &mut [u8], bit_idx: u32) -> bool {
    let byte_idx = (bit_idx / 8) as usize;
    let bit_pos = (bit_idx % 8) as u8;

    if byte_idx >= data.len() {
        return false;
    }

    data[byte_idx] &= !(1 << bit_pos);
    true
}

/// Tests a bit and returns `None` when the bit is out of range.
pub fn test_bit(data: &[u8], bit_idx: u32) -> Option<bool> {
    let byte_idx = (bit_idx / 8) as usize;
    let bit_pos = (bit_idx % 8) as u8;

    if byte_idx >= data.len() {
        return None;
    }

    Some((data[byte_idx] & (1 << bit_pos)) != 0)
}

/// Toggles a bit in-place. Returns `false` if the bit is out of range.
pub fn toggle_bit(data: &mut [u8], bit_idx: u32) -> bool {
    let byte_idx = (bit_idx / 8) as usize;
    let bit_pos = (bit_idx % 8) as u8;

    if byte_idx >= data.len() {
        return false;
    }

    data[byte_idx] ^= 1 << bit_pos;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitmap_utils() {
        assert_eq!(bytes_for_bits(1), 1);
        assert_eq!(bytes_for_bits(8), 1);
        assert_eq!(bytes_for_bits(9), 2);
        assert_eq!(count_set_bits(0b10101010), 4);
    }
}
