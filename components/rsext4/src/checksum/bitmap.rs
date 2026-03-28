//! Block and inode bitmap checksum helpers.

use super::core::ext4_metadata_csum32;
use crate::{crc32c::ext4_crc32c_seed_from_superblock, superblock::Ext4Superblock};

/// Computes the checksum stored for a block bitmap.
pub fn ext4_block_bitmap_csum32(sb: &Ext4Superblock, bitmap_bytes: &[u8]) -> u32 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let size = core::cmp::min((sb.s_clusters_per_group as usize) / 8, bitmap_bytes.len());
    ext4_metadata_csum32(seed, &[&bitmap_bytes[..size]])
}

/// Computes the checksum stored for an inode bitmap.
pub fn ext4_inode_bitmap_csum32(sb: &Ext4Superblock, bitmap_bytes: &[u8]) -> u32 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let size = core::cmp::min((sb.s_inodes_per_group as usize) / 8, bitmap_bytes.len());
    ext4_metadata_csum32(seed, &[&bitmap_bytes[..size]])
}
