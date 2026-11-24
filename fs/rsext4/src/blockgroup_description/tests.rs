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

/// Writes `descs` into one table of `desc_size`-byte records that starts one
/// byte into its buffer, so the table is not aligned for `Ext4GroupDesc`.
fn unaligned_table(descs: &[Ext4GroupDesc], desc_size: usize) -> alloc::vec::Vec<u8> {
    let mut buffer = alloc::vec![0u8; 1 + descs.len() * desc_size];
    for (index, desc) in descs.iter().enumerate() {
        let start = 1 + index * desc_size;
        desc.encode_checked(&mut buffer[start..start + desc_size])
            .unwrap();
    }
    buffer
}

#[test]
fn descriptor_table_reads_and_updates_32_byte_records_in_place() {
    use super::{BlockGroupDescTable, BlockGroupDescTableMut};
    use crate::bmalloc::BGIndex;

    let descs = [
        Ext4GroupDesc {
            bg_block_bitmap_lo: 0x0000_0101,
            bg_free_blocks_count_lo: 5,
            ..Default::default()
        },
        Ext4GroupDesc {
            bg_block_bitmap_lo: 0xffff_ffff,
            bg_inode_bitmap_lo: 0xffff_ffff,
            bg_inode_table_lo: 0xffff_ffff,
            bg_free_blocks_count_lo: 9,
            bg_used_dirs_count_lo: 3,
            ..Default::default()
        },
    ];
    let mut buffer = unaligned_table(&descs, Ext4GroupDesc::GOOD_OLD_DESC_SIZE);
    let second_record = buffer[33..65].to_vec();

    let table = BlockGroupDescTable::new(&buffer[1..], Ext4GroupDesc::GOOD_OLD_DESC_SIZE, 2);
    // A 32-byte record has no high halves: the next record must not be read as them.
    assert_eq!(
        table
            .get_desc(BGIndex::new(0))
            .map(|desc| desc.block_bitmap()),
        Some(0x0000_0101)
    );
    // The last record of an exactly sized table is still inside the table.
    assert_eq!(
        table
            .get_desc(BGIndex::new(1))
            .map(|desc| desc.used_dirs_count()),
        Some(3)
    );
    assert_eq!(table.iter().count(), 2);
    assert_eq!(table.total_free_blocks(), 14);

    let mut mutable_table =
        BlockGroupDescTableMut::new(&mut buffer[1..], Ext4GroupDesc::GOOD_OLD_DESC_SIZE, 2);
    assert!(mutable_table.update_free_blocks(BGIndex::new(0), 7));
    assert!(mutable_table.increment_used_dirs(BGIndex::new(0)));
    assert!(mutable_table.update_used_dirs(BGIndex::new(1), 4));
    // Updating the first record leaves every byte of the second one alone,
    // apart from the field written to it on purpose.
    let mut expected_second = second_record;
    expected_second[16..18].copy_from_slice(&4u16.to_le_bytes());
    assert_eq!(&buffer[33..65], &expected_second[..]);

    let table = BlockGroupDescTable::new(&buffer[1..], Ext4GroupDesc::GOOD_OLD_DESC_SIZE, 2);
    let first = table.get_desc(BGIndex::new(0)).unwrap();
    assert_eq!(first.free_blocks_count(), 7);
    assert_eq!(first.used_dirs_count(), 1);
}

#[test]
fn descriptor_table_reads_and_updates_64_byte_records_in_place() {
    use super::{BlockGroupDescTable, BlockGroupDescTableMut};
    use crate::bmalloc::BGIndex;

    let descs = [
        Ext4GroupDesc {
            bg_block_bitmap_lo: 0x1234_5678,
            bg_block_bitmap_hi: 0x0000_00ab,
            bg_free_blocks_count_lo: 1,
            ..Default::default()
        },
        Ext4GroupDesc {
            bg_free_blocks_count_lo: 2,
            bg_free_blocks_count_hi: 1,
            bg_used_dirs_count_lo: 6,
            ..Default::default()
        },
    ];
    let mut buffer = unaligned_table(&descs, Ext4GroupDesc::EXT4_DESC_SIZE_64BIT);
    let second_record = buffer[65..129].to_vec();

    let table = BlockGroupDescTable::new(&buffer[1..], Ext4GroupDesc::EXT4_DESC_SIZE_64BIT, 2);
    assert_eq!(
        table
            .get_desc(BGIndex::new(0))
            .map(|desc| desc.block_bitmap()),
        Some(0x0000_00ab_1234_5678)
    );
    assert_eq!(table.total_free_blocks(), 1 + 0x1_0002);

    let mut mutable_table =
        BlockGroupDescTableMut::new(&mut buffer[1..], Ext4GroupDesc::EXT4_DESC_SIZE_64BIT, 2);
    assert!(mutable_table.update_free_blocks(BGIndex::new(0), 0x3_0004));
    assert!(mutable_table.set_flags(BGIndex::new(0), Ext4GroupDesc::EXT4_BG_INODE_ZEROED));
    assert_eq!(&buffer[65..129], &second_record[..]);

    let table = BlockGroupDescTable::new(&buffer[1..], Ext4GroupDesc::EXT4_DESC_SIZE_64BIT, 2);
    let first = table.get_desc(BGIndex::new(0)).unwrap();
    assert_eq!(first.free_blocks_count(), 0x3_0004);
    assert!(first.is_inode_table_zeroed());
    assert_eq!(first.block_bitmap(), 0x0000_00ab_1234_5678);
}
