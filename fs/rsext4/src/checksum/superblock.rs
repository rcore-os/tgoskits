//! Superblock checksum helpers.

use crate::{
    crc32c::{crc32c_append, crc32c_init, ext4_superblock_has_metadata_csum},
    endian::DiskFormat,
    superblock::Ext4Superblock,
};

/// Computes the `metadata_csum` checksum stored in `s_checksum`.
pub fn ext4_superblock_csum32(sb: &Ext4Superblock) -> u32 {
    let mut sb_bytes = [0u8; Ext4Superblock::SUPERBLOCK_SIZE];
    sb.to_disk_bytes(&mut sb_bytes);
    let offset = Ext4Superblock::SUPERBLOCK_SIZE - 4;
    crc32c_append(crc32c_init(), &sb_bytes[..offset])
}

/// Updates `s_checksum` when the filesystem enables `metadata_csum`.
pub fn ext4_update_superblock_checksum(sb: &mut Ext4Superblock) {
    if ext4_superblock_has_metadata_csum(sb) {
        sb.s_checksum = ext4_superblock_csum32(sb);
    }
}
