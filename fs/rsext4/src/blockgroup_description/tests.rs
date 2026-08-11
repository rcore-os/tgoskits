//! Tests for block group descriptor helpers.

use super::Ext4GroupDesc;
use crate::{Ext4ErrorKind, endian::DiskFormat, superblock::Ext4Superblock};

#[test]
fn checked_group_desc_decode_rejects_invalid_record_sizes() {
    for len in [0, 31, 33, 63, 65, 96, 2048] {
        let bytes = alloc::vec![0u8; len];
        let error = Ext4GroupDesc::decode_checked(&bytes)
            .expect_err("invalid group descriptor size must be rejected");
        assert_eq!(error.kind(), Ext4ErrorKind::Corrupted, "length {len}");
    }
}

#[test]
fn checked_group_desc_decode_accepts_linux_record_sizes() {
    let short = Ext4GroupDesc::decode_checked(&[0u8; 32]).unwrap();
    assert_eq!(short.bg_block_bitmap_hi, 0);

    let source = Ext4GroupDesc {
        bg_block_bitmap_lo: 0x1234_5678,
        bg_block_bitmap_hi: 0xabcd_ef00,
        ..Default::default()
    };
    let mut extended = alloc::vec![0xa5; 128];
    source.to_disk_bytes(&mut extended);
    let decoded = Ext4GroupDesc::decode_checked(&extended).unwrap();
    assert_eq!(decoded.block_bitmap(), 0xabcd_ef00_1234_5678);
    assert!(extended[64..].iter().all(|byte| *byte == 0xa5));
}

#[test]
fn extended_group_desc_checksum_covers_and_preserves_reserved_tail() {
    let superblock = Ext4Superblock {
        s_feature_incompat: Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT,
        s_feature_ro_compat: Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM,
        s_desc_size: 128,
        ..Default::default()
    };
    let mut desc = Ext4GroupDesc {
        bg_block_bitmap_lo: 42,
        ..Default::default()
    };
    let mut raw_record = alloc::vec![0xa5; 128];

    desc.encode_with_checksum(&superblock, 7, &mut raw_record, None, None)
        .unwrap();

    assert!(raw_record[64..].iter().all(|byte| *byte == 0xa5));
    desc.verify_checksum_in_bytes(&superblock, 7, &raw_record)
        .unwrap();
    raw_record[127] ^= 1;
    assert_eq!(
        desc.verify_checksum_in_bytes(&superblock, 7, &raw_record)
            .unwrap_err()
            .kind(),
        Ext4ErrorKind::ChecksumMismatch
    );
}

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
