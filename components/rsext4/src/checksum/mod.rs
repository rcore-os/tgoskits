//! Ext4 and JBD2 checksum helpers.

mod bitmap;
mod core;
mod dirblock;
mod inode;
mod journal;
mod superblock;

pub use core::ext4_metadata_csum32;

pub use bitmap::{ext4_block_bitmap_csum32, ext4_inode_bitmap_csum32};
pub use dirblock::{
    ext4_dirblock_csum32, ext4_metadata_block_csum32, ext4_update_dirblock_tail_checksum,
    update_ext4_dirblock_csum32, verify_ext4_dirblock_checksum,
};
pub use inode::{ext4_inode_csum32, ext4_update_inode_checksum};
pub use journal::{jbd2_superblock_csum32, jbd2_update_superblock_checksum};
pub use superblock::{ext4_superblock_csum32, ext4_update_superblock_checksum};

/// Computes the 16-bit checksum stored in a group descriptor.
pub fn ext4_group_desc_csum16(
    sb: &crate::superblock::Ext4Superblock,
    group_id: u32,
    desc_bytes: &[u8],
) -> u16 {
    let seed = crate::crc32c::ext4_crc32c_seed_from_superblock(sb);
    let group_id_le = group_id.to_le_bytes();
    let checksum = ext4_metadata_csum32(seed, &[&group_id_le, desc_bytes]);
    (checksum & 0xFFFF) as u16
}

#[cfg(test)]
mod tests;
