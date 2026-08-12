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
    update_ext4_dirblock_csum32, verify_ext4_dirblock_checksum, verify_ext4_dx_checksum,
};
pub(crate) use inode::ext4_update_raw_inode_checksum;
pub use inode::{ext4_inode_csum32, ext4_update_inode_checksum};
pub(crate) use journal::{
    jbd2_commit_block_csum32, jbd2_compat_checksum_append, jbd2_descriptor_block_csum32,
    jbd2_tag_csum32,
};
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

/// Computes a group descriptor checksum while treating the stored checksum
/// field as zero, exactly as Linux ext4 does during verification.
pub(crate) fn ext4_group_desc_csum16_zeroed(
    sb: &crate::superblock::Ext4Superblock,
    group_id: u32,
    desc_bytes: &[u8],
) -> Option<u16> {
    let before_checksum = desc_bytes.get(..30)?;
    let after_checksum = desc_bytes.get(32..)?;
    let seed = crate::crc32c::ext4_crc32c_seed_from_superblock(sb);
    let group_id_le = group_id.to_le_bytes();
    let zero_checksum = [0u8; 2];
    let checksum = ext4_metadata_csum32(
        seed,
        &[
            &group_id_le,
            before_checksum,
            &zero_checksum,
            after_checksum,
        ],
    );
    Some((checksum & 0xFFFF) as u16)
}

#[cfg(test)]
mod tests;
