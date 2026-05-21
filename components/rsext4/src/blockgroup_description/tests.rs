//! Tests for block group descriptor helpers.

use super::Ext4GroupDesc;
use crate::superblock::Ext4Superblock;

#[test]
fn test_group_desc_64bit_values() {
    let desc = Ext4GroupDesc {
        bg_block_bitmap_lo: 0x12345678,
        bg_block_bitmap_hi: 0xABCDEF00,
        bg_inode_bitmap_lo: 0,
        bg_inode_bitmap_hi: 0,
        bg_inode_table_lo: 0,
        bg_inode_table_hi: 0,
        bg_free_blocks_count_lo: 100,
        bg_free_blocks_count_hi: 0,
        bg_free_inodes_count_lo: 200,
        bg_free_inodes_count_hi: 0,
        bg_used_dirs_count_lo: 10,
        bg_used_dirs_count_hi: 0,
        bg_flags: 0,
        bg_exclude_bitmap_lo: 0,
        bg_block_bitmap_csum_lo: 0,
        bg_inode_bitmap_csum_lo: 0,
        bg_itable_unused_lo: 0,
        bg_checksum: 0,
        bg_exclude_bitmap_hi: 0,
        bg_block_bitmap_csum_hi: 0,
        bg_inode_bitmap_csum_hi: 0,
        bg_itable_unused_hi: 0,
        bg_reserved: 0,
    };

    assert_eq!(desc.block_bitmap(), 0xABCDEF0012345678);
    assert_eq!(desc.free_blocks_count(), 100);
    assert_eq!(desc.free_inodes_count(), 200);
    assert_eq!(desc.used_dirs_count(), 10);
}

#[test]
fn test_group_desc_flags() {
    let desc = Ext4GroupDesc {
        bg_flags: Ext4GroupDesc::EXT4_BG_INODE_UNINIT,
        ..Default::default()
    };

    assert!(desc.is_inode_bitmap_uninit());
    assert!(!desc.is_block_bitmap_uninit());
}

#[test]
fn bitmap_checksum_getters_follow_descriptor_width() {
    let desc = Ext4GroupDesc {
        bg_block_bitmap_csum_lo: 0x7217,
        bg_block_bitmap_csum_hi: 0x0d12,
        bg_inode_bitmap_csum_lo: 0x1234,
        bg_inode_bitmap_csum_hi: 0xabcd,
        ..Default::default()
    };

    let mut old_desc_sb = Ext4Superblock {
        s_desc_size: Ext4GroupDesc::GOOD_OLD_DESC_SIZE as u16,
        ..Default::default()
    };
    old_desc_sb.s_feature_incompat &= !Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT;

    let new_desc_sb = Ext4Superblock {
        s_desc_size: Ext4GroupDesc::EXT4_DESC_SIZE_64BIT as u16,
        ..Default::default()
    };

    assert_eq!(desc.block_bitmap_csum(&old_desc_sb), 0x7217);
    assert_eq!(desc.inode_bitmap_csum(&old_desc_sb), 0x1234);
    assert_eq!(desc.block_bitmap_csum(&new_desc_sb), 0x0d127217);
    assert_eq!(desc.inode_bitmap_csum(&new_desc_sb), 0xabcd1234);
}
