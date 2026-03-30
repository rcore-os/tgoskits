//! Shared CRC32C helpers for ext4 metadata checksum calculations.

use crate::crc32c::crc32c_append;

/// Computes the raw ext4 metadata CRC32C by appending each part in order.
pub fn ext4_metadata_csum32(seed: u32, parts: &[&[u8]]) -> u32 {
    let mut crc = seed;
    for part in parts {
        crc = crc32c_append(crc, part);
    }
    crc
}
