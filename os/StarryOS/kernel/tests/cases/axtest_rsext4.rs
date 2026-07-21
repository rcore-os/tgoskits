use alloc::string::ToString;

use axtest::prelude::*;

#[axtest::def_test]
fn rsext4_crc_and_error_rules_hold() {
    use rsext4::{
        Errno, Ext4Error,
        crc32c::{
            crc32c, crc32c_append, crc32c_finalize, crc32c_init, ext4_crc32c_seed_from_superblock,
            ext4_superblock_has_metadata_csum,
        },
        superblock::Ext4Superblock,
    };

    ax_assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    ax_assert_eq!(crc32c(b""), 0);
    let mut crc = crc32c_init();
    crc = crc32c_append(crc, b"hello ");
    crc = crc32c_append(crc, b"ext4");
    ax_assert_eq!(crc32c_finalize(crc), crc32c(b"hello ext4"));

    let mut superblock = Ext4Superblock::default();
    superblock.s_uuid = [0x5a; 16];
    ax_assert!(!ext4_superblock_has_metadata_csum(&superblock));
    ax_assert_eq!(
        ext4_crc32c_seed_from_superblock(&superblock),
        crc32c_append(crc32c_init(), &superblock.s_uuid)
    );
    superblock.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM;
    superblock.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_CSUM_SEED;
    superblock.s_checksum_seed = 0xA1B2_C3D4;
    ax_assert!(ext4_superblock_has_metadata_csum(&superblock));
    ax_assert_eq!(ext4_crc32c_seed_from_superblock(&superblock), 0xA1B2_C3D4);

    ax_assert_eq!(Errno::EINVAL.as_i32(), 22);
    ax_assert_eq!(Errno::EPERM.as_i32(), 1);
    ax_assert_eq!(Errno::EIO.as_i32(), 5);
    ax_assert_eq!(Errno::ENOSPC.as_i32(), 28);
    ax_assert_eq!(Errno::EOPNOTSUPP.as_i32(), 95);
    ax_assert_eq!(Errno::ETIMEDOUT.as_i32(), 110);
    ax_assert_eq!(Errno::EUCLEAN.as_i32(), 117);
    ax_assert_eq!(Errno::from_i32(22), Some(Errno::EINVAL));
    ax_assert_eq!(Errno::from_i32(999), None);
    ax_assert_eq!(Errno::EWOULDBLOCK.as_i32(), Errno::EAGAIN.as_i32());
    ax_assert_eq!(Errno::EINVAL.name(), "EINVAL");
    ax_assert!(Errno::EINVAL.description().contains("Invalid"));

    let error = Ext4Error::buffer_too_small(4, 8);
    ax_assert_eq!(error.code, Errno::EINVAL);
    ax_assert!(error.to_string().contains("provided=4"));
    ax_assert_eq!(Ext4Error::permission_denied().code, Errno::EACCES);
    ax_assert!(
        Ext4Error::block_out_of_range(3, 2)
            .to_string()
            .contains("block_id=3")
    );
    ax_assert!(
        Ext4Error::invalid_block_size(1024, 4096)
            .to_string()
            .contains("expected=4096")
    );
    ax_assert!(
        Ext4Error::alignment(3, 4)
            .to_string()
            .contains("alignment=4")
    );
    ax_assert!(Ext4Error::invalid_input().to_string().contains("EINVAL"));

    let error_cases = [
        (Ext4Error::not_found(), Errno::ENOENT),
        (Ext4Error::already_exists(), Errno::EEXIST),
        (Ext4Error::not_dir(), Errno::ENOTDIR),
        (Ext4Error::is_dir(), Errno::EISDIR),
        (Ext4Error::io(), Errno::EIO),
        (Ext4Error::badf(), Errno::EBADF),
        (Ext4Error::busy(), Errno::EBUSY),
        (Ext4Error::not_empty(), Errno::ENOTEMPTY),
        (Ext4Error::no_space(), Errno::ENOSPC),
        (Ext4Error::read_only(), Errno::EROFS),
        (Ext4Error::unsupported(), Errno::EOPNOTSUPP),
        (Ext4Error::timeout(), Errno::ETIMEDOUT),
        (Ext4Error::corrupted(), Errno::EUCLEAN),
        (Ext4Error::checksum(), Errno::EUCLEAN),
        (Ext4Error::bad_superblock(), Errno::EINVAL),
        (Ext4Error::invalid_magic(), Errno::EINVAL),
        (Ext4Error::already_mounted(), Errno::EBUSY),
    ];
    for (error, errno) in error_cases {
        ax_assert_eq!(error.code, errno);
        ax_assert!(error.context.is_none());
    }
    let operated = Ext4Error::from(Errno::EIO).with_operation("read_inode");
    ax_assert!(operated.to_string().contains("op=read_inode"));
}

#[axtest::def_test]
fn rsext4_superblock_geometry_rules_hold() {
    use rsext4::{GROUP_DESC_SIZE, GROUP_DESC_SIZE_OLD, superblock::Ext4Superblock};

    let mut superblock = Ext4Superblock {
        s_magic: Ext4Superblock::EXT4_SUPER_MAGIC,
        s_log_block_size: 2,
        s_blocks_count_lo: 16_385,
        s_free_blocks_count_lo: 7,
        s_free_blocks_count_hi: 2,
        s_r_blocks_count_lo: 5,
        s_r_blocks_count_hi: 3,
        s_blocks_per_group: 8_192,
        s_inodes_per_group: 8_192,
        s_inode_size: 256,
        s_desc_size: 64,
        s_feature_compat: 0,
        ..Default::default()
    };

    ax_assert!(superblock.is_valid());
    ax_assert_eq!(superblock.block_size(), 4096);
    ax_assert_eq!(superblock.blocks_count(), 16_385);
    ax_assert_eq!(superblock.free_blocks_count(), (2_u64 << 32) | 7);
    ax_assert_eq!(superblock.reserved_blocks_count(), (3_u64 << 32) | 5);
    ax_assert_eq!(superblock.block_groups_count(), 3);
    ax_assert_eq!(superblock.blocks_per_group(), 8_192);
    ax_assert_eq!(superblock.inodes_per_group(), 8_192);
    ax_assert_eq!(superblock.inode_size(), 256);
    ax_assert_eq!(superblock.get_desc_size(), 64);
    ax_assert_eq!(superblock.descs_per_block(), 64);
    ax_assert_eq!(superblock.inode_table_blocks(), 512);

    superblock.s_desc_size = 0;
    ax_assert_eq!(superblock.get_desc_size(), GROUP_DESC_SIZE_OLD);
    superblock.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT;
    ax_assert_eq!(superblock.get_desc_size(), GROUP_DESC_SIZE);
    ax_assert!(superblock.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT));
    ax_assert!(!superblock.has_journal());
    superblock.s_feature_compat |= Ext4Superblock::EXT4_FEATURE_COMPAT_HAS_JOURNAL;
    ax_assert!(superblock.has_journal());
}

#[axtest::def_test]
fn rsext4_inode_extent_timestamp_rules_hold() {
    use rsext4::disknode::{Ext4Extent, Ext4Inode, Ext4TimeSpec, Ext4Timestamp};

    let timestamp = Ext4Timestamp::new(17, 1_500_000_000);
    ax_assert_eq!(timestamp.nsec, Ext4Timestamp::MAX_NSEC);
    ax_assert_eq!(Ext4Timestamp::UNIX_EPOCH, Ext4Timestamp { sec: 0, nsec: 0 });
    ax_assert_eq!(Ext4TimeSpec::Set(timestamp), Ext4TimeSpec::Set(timestamp));
    ax_assert_eq!(Ext4TimeSpec::default(), Ext4TimeSpec::Omit);

    let extent = Ext4Extent::new(4, 0x1234_5678_9abc, 16);
    ax_assert_eq!(extent.ee_block, 4);
    ax_assert_eq!(extent.start_block(), 0x1234_5678_9abc);
    ax_assert_eq!(extent.len(), 16);
    ax_assert!(extent.is_initialized());
    ax_assert!(!extent.is_unwritten());
    ax_assert_eq!(Ext4Extent::decode_len(Ext4Extent::EXT_INIT_MAX_LEN), 32_768);
    ax_assert_eq!(Ext4Extent::encode_len(32_768, false), Some(32_768));
    ax_assert_eq!(Ext4Extent::encode_len(32_768, true), None);
    let unwritten_len = Ext4Extent::encode_len(7, true).unwrap();
    let unwritten_extent = Ext4Extent {
        ee_len: unwritten_len,
        ..extent
    };
    ax_assert!(unwritten_extent.is_unwritten());
    ax_assert_eq!(
        unwritten_extent.build_len_like(8),
        Ext4Extent::encode_len(8, true)
    );

    let mut inode = Ext4Inode::empty_for_reuse(32);
    inode.set_uid(0x1234_5678);
    inode.set_gid(0x9abc_def0);
    ax_assert_eq!(inode.uid(), 0x1234_5678);
    ax_assert_eq!(inode.gid(), 0x9abc_def0);

    inode.set_mode_full(Ext4Inode::S_IFREG | Ext4Inode::S_ISUID | Ext4Inode::S_ISGID | 0o755);
    ax_assert!(inode.is_file());
    ax_assert!(inode.is_executable());
    ax_assert_eq!(inode.permissions(), 0o6755);
    inode.set_mode_preserve_type(0o640);
    ax_assert!(inode.is_file());
    ax_assert_eq!(inode.permissions(), 0o640);

    inode.set_mtime_ts(Ext4Inode::LARGE_INODE_SIZE, timestamp);
    ax_assert_eq!(inode.mtime_ts(Ext4Inode::LARGE_INODE_SIZE), timestamp);
    inode.write_extend_header();
    inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    ax_assert!(inode.have_extend_header_and_use_extend());

    let flags = Ext4Inode::EXT4_DIRSYNC_FL | Ext4Inode::EXT4_TOPDIR_FL | Ext4Inode::EXT4_NOATIME_FL;
    ax_assert_eq!(
        Ext4Inode::mask_flags_for_mode(Ext4Inode::S_IFDIR, flags),
        flags
    );
    ax_assert_eq!(
        Ext4Inode::mask_flags_for_mode(Ext4Inode::S_IFREG, flags),
        Ext4Inode::EXT4_NOATIME_FL
    );
    ax_assert!(Ext4Inode::required_extra_isize(Ext4Inode::FIELD_END_I_PROJID) <= 32);
    ax_assert_eq!(Ext4Inode::max_extra_isize(Ext4Inode::LARGE_INODE_SIZE), 128);
}

#[axtest::def_test]
fn rsext4_bitmap_blockgroup_rules_hold() {
    use rsext4::{
        bitmap::bitmap_utils::{
            bytes_for_bits, clear_bit, count_set_bits, count_set_bits_in_bitmap, set_bit, test_bit,
            toggle_bit,
        },
        blockgroup_description::{BlockGroupStats, Ext4GroupDesc},
        bmalloc::BGIndex,
        superblock::Ext4Superblock,
    };

    ax_assert_eq!(bytes_for_bits(0), 0);
    ax_assert_eq!(bytes_for_bits(1), 1);
    ax_assert_eq!(bytes_for_bits(8), 1);
    ax_assert_eq!(bytes_for_bits(9), 2);
    ax_assert_eq!(count_set_bits(0b1010_1010), 4);
    ax_assert_eq!(count_set_bits_in_bitmap(&[0b1111_0000, 0b1010_1010], 12), 6);
    ax_assert_eq!(count_set_bits_in_bitmap(&[0xff], 4), 4);

    let mut bits = [0u8; 2];
    ax_assert!(set_bit(&mut bits, 0));
    ax_assert!(set_bit(&mut bits, 9));
    ax_assert!(!set_bit(&mut bits, 16));
    ax_assert_eq!(test_bit(&bits, 0), Some(true));
    ax_assert_eq!(test_bit(&bits, 1), Some(false));
    ax_assert_eq!(test_bit(&bits, 16), None);
    ax_assert!(toggle_bit(&mut bits, 1));
    ax_assert_eq!(test_bit(&bits, 1), Some(true));
    ax_assert!(clear_bit(&mut bits, 9));
    ax_assert_eq!(test_bit(&bits, 9), Some(false));
    ax_assert!(!clear_bit(&mut bits, 16));
    ax_assert!(!toggle_bit(&mut bits, 16));

    let desc = Ext4GroupDesc {
        bg_block_bitmap_lo: 0x89ab_cdef,
        bg_block_bitmap_hi: 0x0123_4567,
        bg_inode_bitmap_lo: 0x7654_3210,
        bg_inode_bitmap_hi: 0xfedc_ba98,
        bg_inode_table_lo: 0x1111_2222,
        bg_inode_table_hi: 0x3333_4444,
        bg_free_blocks_count_lo: 7,
        bg_free_blocks_count_hi: 2,
        bg_free_inodes_count_lo: 9,
        bg_free_inodes_count_hi: 1,
        bg_used_dirs_count_lo: 11,
        bg_used_dirs_count_hi: 3,
        bg_itable_unused_lo: 13,
        bg_itable_unused_hi: 4,
        bg_exclude_bitmap_lo: 0x5555_aaaa,
        bg_exclude_bitmap_hi: 0xaaaa_5555,
        bg_block_bitmap_csum_lo: 0xbeef,
        bg_block_bitmap_csum_hi: 0x1234,
        bg_inode_bitmap_csum_lo: 0xabcd,
        bg_inode_bitmap_csum_hi: 0x5678,
        bg_flags: Ext4GroupDesc::EXT4_BG_BLOCK_UNINIT
            | Ext4GroupDesc::EXT4_BG_INODE_UNINIT
            | Ext4GroupDesc::EXT4_BG_INODE_ZEROED,
        ..Default::default()
    };
    ax_assert_eq!(desc.block_bitmap(), 0x0123_4567_89ab_cdef);
    ax_assert_eq!(desc.inode_bitmap(), 0xfedc_ba98_7654_3210);
    ax_assert_eq!(desc.inode_table(), 0x3333_4444_1111_2222);
    ax_assert_eq!(desc.free_blocks_count(), (2 << 16) | 7);
    ax_assert_eq!(desc.free_inodes_count(), (1 << 16) | 9);
    ax_assert_eq!(desc.used_dirs_count(), (3 << 16) | 11);
    ax_assert_eq!(desc.itable_unused(), (4 << 16) | 13);
    ax_assert_eq!(desc.exclude_bitmap(), 0xaaaa_5555_5555_aaaa);
    ax_assert!(desc.is_uninit_bg());
    ax_assert!(desc.is_block_bitmap_uninit());
    ax_assert!(desc.is_inode_bitmap_uninit());
    ax_assert!(desc.is_inode_table_zeroed());

    let mut superblock = Ext4Superblock::default();
    superblock.s_desc_size = Ext4GroupDesc::GOOD_OLD_DESC_SIZE as u16;
    ax_assert_eq!(desc.block_bitmap_csum(&superblock), 0xbeef);
    ax_assert!(desc.block_bitmap_csum_matches(&superblock, 0x1111_beef));
    superblock.s_desc_size = Ext4GroupDesc::EXT4_DESC_SIZE_64BIT as u16;
    superblock.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT;
    ax_assert_eq!(desc.block_bitmap_csum(&superblock), 0x1234_beef);
    ax_assert_eq!(desc.inode_bitmap_csum(&superblock), 0x5678_abcd);
    ax_assert!(desc.block_bitmap_csum_matches(&superblock, 0x1234_beef));
    ax_assert!(!desc.inode_bitmap_csum_matches(&superblock, 0x1111_abcd));

    let stats = BlockGroupStats::from_desc(BGIndex::new(5), &desc);
    ax_assert_eq!(stats.group_idx.raw(), 5);
    ax_assert_eq!(stats.free_blocks, (2 << 16) | 7);
    ax_assert_eq!(stats.used_inodes(200_000), 200_000 - ((1 << 16) | 9));
    ax_assert_eq!(stats.used_blocks(100_000), 0);
    ax_assert_eq!(stats.used_blocks(200_000), 200_000 - ((2 << 16) | 7));
    ax_assert_eq!(stats.used_inodes(1), 0);
    ax_assert_eq!(stats.used_blocks(1), 0);
    ax_assert_eq!(stats.block_usage_percent(0), 0.0);
    ax_assert_eq!(stats.inode_usage_percent(0), 0.0);
    ax_assert!(stats.block_usage_percent(200_000) > 0.0);
    ax_assert!(stats.inode_usage_percent(200_000) > 0.0);
}
