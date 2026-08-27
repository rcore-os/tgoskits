//! Disk encoding helpers for block group descriptors.

use super::desc::Ext4GroupDesc;
use crate::{
    endian::{DiskFormat, read_u16_le, read_u32_le, write_u16_le, write_u32_le},
    error::{Ext4Error, Ext4Result},
};

impl Ext4GroupDesc {
    /// Largest group descriptor accepted by Linux ext4.
    pub const MAX_DESC_SIZE: usize = crate::config::GROUP_DESC_SIZE_MAX as usize;

    /// Decodes one validated on-disk group descriptor record.
    ///
    /// Linux uses 32-byte descriptors without the `64bit` feature and a
    /// power-of-two size from 64 through 1024 bytes with it. Fields after the
    /// first 64 bytes are reserved for future extensions and are deliberately
    /// ignored by the typed view.
    pub fn decode_checked(bytes: &[u8]) -> Ext4Result<Self> {
        if !Self::is_supported_record_size(bytes.len()) {
            return Err(Ext4Error::corrupted().with_operation("group_descriptor:record_size"));
        }
        Ok(Self::from_validated_disk_bytes(bytes))
    }

    /// Encodes the known fields while retaining extension bytes beyond byte 64.
    pub(crate) fn encode_checked(&self, bytes: &mut [u8]) -> Ext4Result<()> {
        if !Self::is_supported_record_size(bytes.len()) {
            return Err(Ext4Error::corrupted().with_operation("group_descriptor:record_size"));
        }
        self.write_validated_disk_bytes(bytes);
        Ok(())
    }

    const fn is_supported_record_size(size: usize) -> bool {
        size == Self::GOOD_OLD_DESC_SIZE
            || (size >= Self::EXT4_DESC_SIZE_64BIT
                && size <= Self::MAX_DESC_SIZE
                && size.is_power_of_two())
    }

    fn from_validated_disk_bytes(bytes: &[u8]) -> Self {
        if bytes.len() == Self::GOOD_OLD_DESC_SIZE {
            Self {
                bg_block_bitmap_lo: read_u32_le(&bytes[0..4]),
                bg_inode_bitmap_lo: read_u32_le(&bytes[4..8]),
                bg_inode_table_lo: read_u32_le(&bytes[8..12]),
                bg_free_blocks_count_lo: read_u16_le(&bytes[12..14]),
                bg_free_inodes_count_lo: read_u16_le(&bytes[14..16]),
                bg_used_dirs_count_lo: read_u16_le(&bytes[16..18]),
                bg_flags: read_u16_le(&bytes[18..20]),
                bg_exclude_bitmap_lo: read_u32_le(&bytes[20..24]),
                bg_block_bitmap_csum_lo: read_u16_le(&bytes[24..26]),
                bg_inode_bitmap_csum_lo: read_u16_le(&bytes[26..28]),
                bg_itable_unused_lo: read_u16_le(&bytes[28..30]),
                bg_checksum: read_u16_le(&bytes[30..32]),
                bg_block_bitmap_hi: 0,
                bg_inode_bitmap_hi: 0,
                bg_inode_table_hi: 0,
                bg_free_blocks_count_hi: 0,
                bg_free_inodes_count_hi: 0,
                bg_used_dirs_count_hi: 0,
                bg_itable_unused_hi: 0,
                bg_exclude_bitmap_hi: 0,
                bg_block_bitmap_csum_hi: 0,
                bg_inode_bitmap_csum_hi: 0,
                bg_reserved: 0,
            }
        } else {
            Self {
                bg_block_bitmap_lo: read_u32_le(&bytes[0..4]),
                bg_inode_bitmap_lo: read_u32_le(&bytes[4..8]),
                bg_inode_table_lo: read_u32_le(&bytes[8..12]),
                bg_free_blocks_count_lo: read_u16_le(&bytes[12..14]),
                bg_free_inodes_count_lo: read_u16_le(&bytes[14..16]),
                bg_used_dirs_count_lo: read_u16_le(&bytes[16..18]),
                bg_flags: read_u16_le(&bytes[18..20]),
                bg_exclude_bitmap_lo: read_u32_le(&bytes[20..24]),
                bg_block_bitmap_csum_lo: read_u16_le(&bytes[24..26]),
                bg_inode_bitmap_csum_lo: read_u16_le(&bytes[26..28]),
                bg_itable_unused_lo: read_u16_le(&bytes[28..30]),
                bg_checksum: read_u16_le(&bytes[30..32]),
                bg_block_bitmap_hi: read_u32_le(&bytes[32..36]),
                bg_inode_bitmap_hi: read_u32_le(&bytes[36..40]),
                bg_inode_table_hi: read_u32_le(&bytes[40..44]),
                bg_free_blocks_count_hi: read_u16_le(&bytes[44..46]),
                bg_free_inodes_count_hi: read_u16_le(&bytes[46..48]),
                bg_used_dirs_count_hi: read_u16_le(&bytes[48..50]),
                bg_itable_unused_hi: read_u16_le(&bytes[50..52]),
                bg_exclude_bitmap_hi: read_u32_le(&bytes[52..56]),
                bg_block_bitmap_csum_hi: read_u16_le(&bytes[56..58]),
                bg_inode_bitmap_csum_hi: read_u16_le(&bytes[58..60]),
                bg_reserved: read_u32_le(&bytes[60..64]),
            }
        }
    }

    fn write_validated_disk_bytes(&self, bytes: &mut [u8]) {
        write_u32_le(self.bg_block_bitmap_lo, &mut bytes[0..4]);
        write_u32_le(self.bg_inode_bitmap_lo, &mut bytes[4..8]);
        write_u32_le(self.bg_inode_table_lo, &mut bytes[8..12]);
        write_u16_le(self.bg_free_blocks_count_lo, &mut bytes[12..14]);
        write_u16_le(self.bg_free_inodes_count_lo, &mut bytes[14..16]);
        write_u16_le(self.bg_used_dirs_count_lo, &mut bytes[16..18]);
        write_u16_le(self.bg_flags, &mut bytes[18..20]);
        write_u32_le(self.bg_exclude_bitmap_lo, &mut bytes[20..24]);
        write_u16_le(self.bg_block_bitmap_csum_lo, &mut bytes[24..26]);
        write_u16_le(self.bg_inode_bitmap_csum_lo, &mut bytes[26..28]);
        write_u16_le(self.bg_itable_unused_lo, &mut bytes[28..30]);
        write_u16_le(self.bg_checksum, &mut bytes[30..32]);

        if bytes.len() >= Self::EXT4_DESC_SIZE_64BIT {
            write_u32_le(self.bg_block_bitmap_hi, &mut bytes[32..36]);
            write_u32_le(self.bg_inode_bitmap_hi, &mut bytes[36..40]);
            write_u32_le(self.bg_inode_table_hi, &mut bytes[40..44]);
            write_u16_le(self.bg_free_blocks_count_hi, &mut bytes[44..46]);
            write_u16_le(self.bg_free_inodes_count_hi, &mut bytes[46..48]);
            write_u16_le(self.bg_used_dirs_count_hi, &mut bytes[48..50]);
            write_u16_le(self.bg_itable_unused_hi, &mut bytes[50..52]);
            write_u32_le(self.bg_exclude_bitmap_hi, &mut bytes[52..56]);
            write_u16_le(self.bg_block_bitmap_csum_hi, &mut bytes[56..58]);
            write_u16_le(self.bg_inode_bitmap_csum_hi, &mut bytes[58..60]);
            write_u32_le(self.bg_reserved, &mut bytes[60..64]);
        }
    }
}

impl DiskFormat for Ext4GroupDesc {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        Self::from_validated_disk_bytes(bytes)
    }

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
        self.write_validated_disk_bytes(bytes);
    }

    fn disk_size() -> usize {
        64
    }
}

#[cfg(test)]
pub(crate) fn block_group_desc_disk_format_rules_hold_for_test() -> bool {
    // DiskFormat for Ext4GroupDesc: disk_size should be 64
    assert!(<Ext4GroupDesc as DiskFormat>::disk_size() == 64);

    // Test from_disk_bytes with 32-byte input (short form)
    let short_bytes = [0u8; 32];
    let desc = Ext4GroupDesc::from_disk_bytes(&short_bytes);
    assert!(desc.bg_block_bitmap_lo == 0);
    assert!(desc.bg_inode_bitmap_lo == 0);
    assert!(desc.bg_inode_table_lo == 0);
    // High parts should be zero for short form
    assert!(desc.bg_block_bitmap_hi == 0);
    assert!(desc.bg_reserved == 0);

    true
}
