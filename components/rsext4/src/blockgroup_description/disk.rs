//! Disk encoding helpers for block group descriptors.

use super::desc::Ext4GroupDesc;
use crate::endian::{DiskFormat, read_u16_le, read_u32_le, write_u16_le, write_u32_le};

impl DiskFormat for Ext4GroupDesc {
    fn from_disk_bytes(bytes: &[u8]) -> Self {
        if bytes.len() == 32 {
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

    fn to_disk_bytes(&self, bytes: &mut [u8]) {
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

        if bytes.len() >= 64 {
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

    fn disk_size() -> usize {
        64
    }
}
