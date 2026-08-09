//! Tests for block group descriptor helpers.

use super::Ext4GroupDesc;

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
