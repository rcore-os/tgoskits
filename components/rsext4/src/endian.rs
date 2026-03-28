//! Endian conversion helpers for ext4 disk structures.
//!
//! ext4 metadata is stored in little-endian byte order on disk. These helpers
//! bridge the in-memory representation and the serialized on-disk layout.

use core::mem::size_of;

/// Reads a `u16` from little-endian bytes.
#[inline]
pub fn read_u16_le(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

/// Reads a `u32` from little-endian bytes.
#[inline]
pub fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Reads a `u64` from little-endian bytes.
#[inline]
pub fn read_u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// Writes a `u16` as little-endian bytes.
#[inline]
pub fn write_u16_le(value: u16, bytes: &mut [u8]) {
    let le_bytes = value.to_le_bytes();
    bytes[0] = le_bytes[0];
    bytes[1] = le_bytes[1];
}

/// Writes a `u32` as little-endian bytes.
#[inline]
pub fn write_u32_le(value: u32, bytes: &mut [u8]) {
    let le_bytes = value.to_le_bytes();
    bytes[0..4].copy_from_slice(&le_bytes);
}

/// Writes a `u64` as little-endian bytes.
#[inline]
pub fn write_u64_le(value: u64, bytes: &mut [u8]) {
    let le_bytes = value.to_le_bytes();
    bytes[0..8].copy_from_slice(&le_bytes);
}

/// Trait for types that can be serialized to and from on-disk byte slices.
pub trait DiskFormat: Sized {
    /// Deserializes from on-disk bytes.
    fn from_disk_bytes(bytes: &[u8]) -> Self;

    /// Serializes into on-disk bytes.
    fn to_disk_bytes(&self, bytes: &mut [u8]);

    /// Returns the serialized on-disk size in bytes.
    fn disk_size() -> usize {
        size_of::<Self>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_u16_conversion() {
        let value = 0x1234u16;
        let mut bytes = [0u8; 2];

        write_u16_le(value, &mut bytes);
        assert_eq!(bytes, [0x34, 0x12]); // little-endian: low byte first

        let read_value = read_u16_le(&bytes);
        assert_eq!(read_value, value);
    }

    #[test]
    fn test_u32_conversion() {
        let value = 0x12345678u32;
        let mut bytes = [0u8; 4];

        write_u32_le(value, &mut bytes);
        assert_eq!(bytes, [0x78, 0x56, 0x34, 0x12]); // little-endian

        let read_value = read_u32_le(&bytes);
        assert_eq!(read_value, value);
    }

    #[test]
    fn test_u64_conversion() {
        let value = 0x123456789ABCDEF0u64;
        let mut bytes = [0u8; 8];

        write_u64_le(value, &mut bytes);
        assert_eq!(bytes, [0xF0, 0xDE, 0xBC, 0x9A, 0x78, 0x56, 0x34, 0x12]);

        let read_value = read_u64_le(&bytes);
        assert_eq!(read_value, value);
    }
}
