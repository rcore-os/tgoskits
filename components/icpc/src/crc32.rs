//! IEEE CRC-32 (poly 0xEDB88320), matching common Ethernet / gzip tables.

const POLY: u32 = 0xEDB8_8320;

/// Computes CRC-32 over `data` (init `0xffff_ffff`, final XOR `0xffff_ffff`).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (POLY & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::crc32;

    #[test]
    fn empty_is_zero() {
        assert_eq!(crc32(&[]), 0);
    }

    #[test]
    fn known_vector_123456789() {
        // Standard check value for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
