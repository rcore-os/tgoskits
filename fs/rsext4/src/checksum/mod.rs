//! Ext4 and JBD2 checksum helpers.

mod bitmap;
mod core;
mod dirblock;
mod inode;
mod journal;
mod superblock;

pub use core::ext4_metadata_csum32;

pub use bitmap::{ext4_block_bitmap_csum32, ext4_inode_bitmap_csum32};
pub(crate) use dirblock::update_ext4_dx_checksum;
pub use dirblock::{
    ext4_dirblock_csum32, ext4_metadata_block_csum32, ext4_update_dirblock_tail_checksum,
    update_ext4_dirblock_csum32, verify_ext4_dirblock_checksum, verify_ext4_dx_checksum,
};
pub(crate) use inode::ext4_update_raw_inode_checksum;
pub use inode::{ext4_inode_csum32, ext4_update_inode_checksum};
pub(crate) use journal::{
    jbd2_commit_block_csum32, jbd2_compat_checksum_append, jbd2_descriptor_block_csum32,
    jbd2_partial_commit_block_csum32, jbd2_tag_csum32,
};
pub use journal::{jbd2_superblock_csum32, jbd2_update_superblock_checksum};
pub use superblock::{ext4_superblock_csum32, ext4_update_superblock_checksum};

/// Computes the 16-bit checksum stored in a group descriptor.
pub fn ext4_group_desc_csum16(
    sb: &crate::superblock::Ext4Superblock,
    group_id: u32,
    desc_bytes: &[u8],
) -> u16 {
    ext4_group_desc_csum16_zeroed(sb, group_id, desc_bytes).unwrap_or(0)
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
    let group_id_le = group_id.to_le_bytes();
    if crate::crc32c::ext4_superblock_has_metadata_csum(sb) {
        let seed = crate::crc32c::ext4_crc32c_seed_from_superblock(sb);
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
        return Some((checksum & 0xFFFF) as u16);
    }

    if !sb.has_feature_ro_compat(crate::superblock::Ext4Superblock::EXT4_FEATURE_RO_COMPAT_GDT_CSUM)
    {
        return Some(0);
    }

    // Linux's legacy `uninit_bg` format uses crc16(poly=0x8005), seeded
    // with ~0 and fed the filesystem UUID, little-endian group number, and
    // descriptor bytes while omitting bg_checksum itself. The descriptor
    // extension participates only on 64-bit filesystems.
    let mut checksum = linux_crc16(u16::MAX, &sb.s_uuid);
    checksum = linux_crc16(checksum, &group_id_le);
    checksum = linux_crc16(checksum, before_checksum);
    if sb.has_feature_incompat(crate::superblock::Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT) {
        checksum = linux_crc16(checksum, after_checksum);
    }
    Some(checksum)
}

fn linux_crc16(mut checksum: u16, bytes: &[u8]) -> u16 {
    for byte in bytes {
        checksum ^= u16::from(*byte);
        for _ in 0..8 {
            checksum = if checksum & 1 != 0 {
                (checksum >> 1) ^ 0xA001
            } else {
                checksum >> 1
            };
        }
    }
    checksum
}

#[cfg(test)]
mod tests;
