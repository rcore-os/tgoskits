//! Inode checksum helpers.

use super::core::ext4_metadata_csum32;
use crate::{
    bmalloc::InodeNumber,
    crc32c::ext4_crc32c_seed_from_superblock,
    disknode::Ext4Inode,
    endian::DiskFormat,
    error::{Ext4Error, Ext4Result},
    superblock::Ext4Superblock,
};

const INODE_CHECKSUM_LO_OFFSET: usize = 124;
const INODE_CHECKSUM_HI_OFFSET: usize = 130;

fn inode_csum_from_raw(
    sb: &Ext4Superblock,
    inode_num: InodeNumber,
    generation: u32,
    inode: &Ext4Inode,
    raw_inode: &[u8],
) -> Ext4Result<u32> {
    if raw_inode.len() < Ext4Inode::GOOD_OLD_INODE_SIZE as usize {
        return Err(Ext4Error::corrupted().with_operation("inode:checksum"));
    }

    let mut inode_bytes = raw_inode.to_vec();
    inode_bytes[INODE_CHECKSUM_LO_OFFSET..INODE_CHECKSUM_LO_OFFSET + 2].fill(0);
    let inode_size = u16::try_from(inode_bytes.len()).unwrap_or(u16::MAX);
    if inode.field_fits(inode_size, Ext4Inode::FIELD_END_I_CHECKSUM_HI) {
        inode_bytes[INODE_CHECKSUM_HI_OFFSET..INODE_CHECKSUM_HI_OFFSET + 2].fill(0);
    }

    let seed = ext4_crc32c_seed_from_superblock(sb);
    let inode_num_le = inode_num.raw().to_le_bytes();
    let generation_le = generation.to_le_bytes();
    Ok(ext4_metadata_csum32(
        seed,
        &[&inode_num_le, &generation_le, &inode_bytes],
    ))
}

/// Computes the full 32-bit inode checksum.
pub fn ext4_inode_csum32(
    sb: &Ext4Superblock,
    inode_num: InodeNumber,
    generation: u32,
    inode: &Ext4Inode,
    inode_size: usize,
) -> Ext4Result<u32> {
    if inode_size < Ext4Inode::GOOD_OLD_INODE_SIZE as usize {
        return Err(Ext4Error::invalid_input().with_operation("inode:checksum"));
    }
    let mut inode_bytes = alloc::vec![0u8; inode_size];
    inode.to_disk_bytes(&mut inode_bytes);
    inode_csum_from_raw(sb, inode_num, generation, inode, &inode_bytes)
}

/// Serializes an inode into its preserved raw record and refreshes its checksum.
pub(crate) fn ext4_update_raw_inode_checksum(
    sb: &Ext4Superblock,
    inode_num: InodeNumber,
    inode: &mut Ext4Inode,
    raw_inode: &mut [u8],
) -> Ext4Result<()> {
    inode.to_disk_bytes(raw_inode);
    let checksum = inode_csum_from_raw(sb, inode_num, inode.i_generation, inode, raw_inode)?;
    inode.l_i_checksum_lo = (checksum & 0xFFFF) as u16;
    raw_inode[INODE_CHECKSUM_LO_OFFSET..INODE_CHECKSUM_LO_OFFSET + 2]
        .copy_from_slice(&inode.l_i_checksum_lo.to_le_bytes());

    let inode_size = u16::try_from(raw_inode.len()).unwrap_or(u16::MAX);
    if inode.field_fits(inode_size, Ext4Inode::FIELD_END_I_CHECKSUM_HI) {
        inode.i_checksum_hi = ((checksum >> 16) & 0xFFFF) as u16;
        raw_inode[INODE_CHECKSUM_HI_OFFSET..INODE_CHECKSUM_HI_OFFSET + 2]
            .copy_from_slice(&inode.i_checksum_hi.to_le_bytes());
    } else {
        inode.i_checksum_hi = 0;
    }
    Ok(())
}

/// Computes and stores the split inode checksum fields.
pub fn ext4_update_inode_checksum(
    sb: &Ext4Superblock,
    inode_num: InodeNumber,
    generation: u32,
    inode: &mut Ext4Inode,
    inode_size: usize,
) -> Ext4Result<()> {
    let checksum = ext4_inode_csum32(sb, inode_num, generation, inode, inode_size)?;
    inode.l_i_checksum_lo = (checksum & 0xFFFF) as u16;
    inode.i_checksum_hi = ((checksum >> 16) & 0xFFFF) as u16;
    Ok(())
}
