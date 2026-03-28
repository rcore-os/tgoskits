//! Inode checksum helpers.

use super::core::ext4_metadata_csum32;
use crate::{
    bmalloc::InodeNumber, crc32c::ext4_crc32c_seed_from_superblock, disknode::Ext4Inode,
    endian::DiskFormat, superblock::Ext4Superblock,
};

/// Computes the full 32-bit inode checksum.
pub fn ext4_inode_csum32(
    sb: &Ext4Superblock,
    inode_num: InodeNumber,
    generation: u32,
    inode: &Ext4Inode,
    inode_size: usize,
) -> u32 {
    let seed = ext4_crc32c_seed_from_superblock(sb);
    let inode_num_le = inode_num.raw().to_le_bytes();
    let generation_le = generation.to_le_bytes();

    let mut inode_bytes = alloc::vec![0u8; inode_size];
    let mut inode_for_csum = *inode;
    inode_for_csum.l_i_checksum_lo = 0;
    inode_for_csum.i_checksum_hi = 0;
    inode_for_csum.to_disk_bytes(&mut inode_bytes);

    ext4_metadata_csum32(seed, &[&inode_num_le, &generation_le, &inode_bytes])
}

/// Computes and stores the split inode checksum fields.
pub fn ext4_update_inode_checksum(
    sb: &Ext4Superblock,
    inode_num: InodeNumber,
    generation: u32,
    inode: &mut Ext4Inode,
    inode_size: usize,
) {
    let checksum = ext4_inode_csum32(sb, inode_num, generation, inode, inode_size);
    inode.l_i_checksum_lo = (checksum & 0xFFFF) as u16;
    inode.i_checksum_hi = ((checksum >> 16) & 0xFFFF) as u16;
}
