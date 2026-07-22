use alloc::{string::ToString, vec, vec::Vec};

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

    let default_header = rsext4::disknode::Ext4ExtentHeader::default();
    ax_assert_eq!(
        default_header.eh_magic,
        rsext4::disknode::Ext4ExtentHeader::EXT4_EXT_MAGIC
    );
    let default_extent = Ext4Extent::default();
    ax_assert_eq!(default_extent.ee_block, 0);
    ax_assert_eq!(default_extent.len(), Ext4Extent::EXT_INIT_MAX_LEN as u32);
    ax_assert!(!default_extent.is_empty());
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
    ax_assert_eq!(Ext4Extent::encode_len(0, false), None);
    ax_assert_eq!(Ext4Extent::encode_len(32_769, false), None);
    ax_assert_eq!(Ext4Extent::encode_len(32_768, true), None);

    let mut inode = Ext4Inode::empty_for_reuse(32);
    inode.set_uid(0x1234_5678);
    inode.set_gid(0x9abc_def0);
    ax_assert_eq!(inode.uid(), 0x1234_5678);
    ax_assert_eq!(inode.gid(), 0x9abc_def0);
    inode.i_file_acl_lo = 0x89ab_cdef;
    inode.l_i_file_acl_high = 0x1234;
    ax_assert_eq!(inode.file_acl(), 0x1234_89ab_cdef);

    inode.set_mode_full(Ext4Inode::S_IFREG | Ext4Inode::S_ISUID | Ext4Inode::S_ISGID | 0o755);
    ax_assert!(inode.is_file());
    ax_assert!(inode.is_executable());
    ax_assert_eq!(inode.permissions(), 0o6755);
    inode.clear_setid_bits_for_content_change();
    ax_assert_eq!(inode.permissions(), 0o0755);
    inode.set_mode_full(Ext4Inode::S_IFREG | Ext4Inode::S_ISUID | Ext4Inode::S_ISGID | 0o644);
    inode.clear_setid_bits_for_content_change();
    ax_assert_eq!(inode.permissions(), 0o2644);
    inode.clear_setid_bits_for_chown();
    ax_assert_eq!(inode.permissions(), 0o0644);
    inode.set_mode_preserve_type(0o640);
    ax_assert!(inode.is_file());
    ax_assert_eq!(inode.permissions(), 0o640);

    let large_time = Ext4Timestamp::new((1_i64 << 33) + 17, 987_654_321);
    inode.set_atime_ts(Ext4Inode::LARGE_INODE_SIZE, large_time);
    inode.set_ctime_ts(Ext4Inode::LARGE_INODE_SIZE, large_time);
    inode.set_mtime_ts(Ext4Inode::LARGE_INODE_SIZE, timestamp);
    inode.set_crtime_ts(Ext4Inode::LARGE_INODE_SIZE, large_time);
    ax_assert_eq!(inode.atime_ts(Ext4Inode::LARGE_INODE_SIZE), large_time);
    ax_assert_eq!(inode.ctime_ts(Ext4Inode::LARGE_INODE_SIZE), large_time);
    ax_assert_eq!(inode.mtime_ts(Ext4Inode::LARGE_INODE_SIZE), timestamp);
    ax_assert_eq!(
        inode.crtime_ts(Ext4Inode::LARGE_INODE_SIZE),
        Some(large_time)
    );
    ax_assert_eq!(inode.crtime_ts(Ext4Inode::GOOD_OLD_INODE_SIZE), None);
    let clamped_time = Ext4Timestamp::new(i64::from(i32::MAX) + 10, 55);
    inode.set_atime_ts(Ext4Inode::GOOD_OLD_INODE_SIZE, clamped_time);
    ax_assert_eq!(
        inode.atime_ts(Ext4Inode::GOOD_OLD_INODE_SIZE),
        Ext4Timestamp::new(i64::from(i32::MAX), 0)
    );
    inode.set_crtime_ts(Ext4Inode::GOOD_OLD_INODE_SIZE, clamped_time);
    ax_assert_eq!(inode.i_crtime, 0);
    inode.write_extend_header();
    inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    ax_assert!(inode.have_extend_header_and_use_extend());
    inode.i_block[0] = 0;
    ax_assert!(!inode.have_extend_header_and_use_extend());

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

#[axtest::def_test]
fn rsext4_entries_and_directory_iterator_rules_hold() {
    use rsext4::{
        DIRNAME_LEN,
        endian::DiskFormat,
        entries::{
            DirEntryIterator, Ext4DirEntry2, Ext4DirEntryInfo, Ext4DirEntryTail, Ext4ExtentStatus,
            htree_dir,
        },
    };

    let entry = Ext4DirEntry2::new(
        42,
        Ext4DirEntry2::entry_len(5),
        Ext4DirEntry2::EXT4_FT_REG_FILE,
        b"alpha",
    );
    ax_assert_eq!(entry.inode, 42);
    ax_assert_eq!(entry.name_len, 5);
    ax_assert_eq!(&entry.name[..5], b"alpha");
    ax_assert_eq!(Ext4DirEntry2::entry_len(0), 8);
    ax_assert_eq!(Ext4DirEntry2::entry_len(1), 12);
    ax_assert_eq!(Ext4DirEntry2::entry_len(4), 12);
    ax_assert_eq!(Ext4DirEntry2::entry_len(5), 16);

    let long_name = [b'x'; DIRNAME_LEN + 8];
    let truncated = Ext4DirEntry2::new(7, 264, Ext4DirEntry2::EXT4_FT_DIR, &long_name);
    ax_assert_eq!(truncated.name_len, Ext4DirEntry2::MAX_NAME_LEN);

    let mut header_bytes = [0_u8; 8];
    entry.to_disk_bytes(&mut header_bytes);
    ax_assert_eq!(Ext4DirEntry2::disk_size(), 8);
    let parsed_header = Ext4DirEntry2::from_disk_bytes(&header_bytes);
    ax_assert_eq!(parsed_header.inode, entry.inode);
    ax_assert_eq!(parsed_header.rec_len, entry.rec_len);
    ax_assert_eq!(parsed_header.name_len, entry.name_len);
    ax_assert_eq!(parsed_header.file_type, entry.file_type);

    let tail = Ext4DirEntryTail::new();
    ax_assert_eq!(tail.det_rec_len, Ext4DirEntryTail::TAIL_LEN);
    ax_assert_eq!(tail.det_reserved_ft, Ext4DirEntryTail::RESERVED_FT);
    let mut tail_bytes = [0_u8; 12];
    let mut tail = tail;
    tail.det_checksum = 0x1234_5678;
    tail.to_disk_bytes(&mut tail_bytes);
    let parsed_tail = Ext4DirEntryTail::from_disk_bytes(&tail_bytes);
    ax_assert_eq!(parsed_tail.det_checksum, 0x1234_5678);
    ax_assert_eq!(Ext4DirEntryTail::disk_size(), 12);

    let mut block = vec![0_u8; 64];
    write_dirent(&mut block[0..12], 2, 12, Ext4DirEntry2::EXT4_FT_DIR, b".");
    write_dirent(&mut block[12..24], 2, 12, Ext4DirEntry2::EXT4_FT_DIR, b"..");
    write_dirent(
        &mut block[24..40],
        11,
        16,
        Ext4DirEntry2::EXT4_FT_REG_FILE,
        b"file",
    );
    write_dirent(
        &mut block[40..48],
        0,
        8,
        Ext4DirEntry2::EXT4_FT_UNKNOWN,
        b"",
    );

    let entries: Vec<_> = DirEntryIterator::new(&block).collect();
    ax_assert_eq!(entries.len(), 3);
    ax_assert!(entries[0].0.is_dot());
    ax_assert!(entries[1].0.is_dotdot());
    ax_assert_eq!(entries[2].0.name_str(), Some("file"));
    ax_assert_eq!(entries[2].1, 16);

    let found = rsext4::entries::classic_dir::find_entry(&block, b"file").unwrap();
    ax_assert_eq!(found.inode, 11);
    ax_assert_eq!(found.file_type, Ext4DirEntry2::EXT4_FT_REG_FILE);
    ax_assert!(rsext4::entries::classic_dir::find_entry(&block, b"missing").is_none());
    ax_assert_eq!(rsext4::entries::classic_dir::list_entries(&block).len(), 3);

    ax_assert!(Ext4DirEntryInfo::parse_from_bytes(&[0; 7]).is_none());
    let mut bad = [0_u8; 12];
    write_dirent(&mut bad, 0, 12, Ext4DirEntry2::EXT4_FT_REG_FILE, b"x");
    ax_assert!(Ext4DirEntryInfo::parse_from_bytes(&bad).is_none());
    write_dirent(&mut bad, 1, 7, Ext4DirEntry2::EXT4_FT_REG_FILE, b"x");
    ax_assert!(Ext4DirEntryInfo::parse_from_bytes(&bad).is_none());

    let seed = [1, 2, 3, 4];
    ax_assert_eq!(
        htree_dir::calculate_hash(b"abc", htree_dir::Ext4DxRootInfo::DX_HASH_LEGACY, &seed),
        ((b'a' as u32) * 33 + b'b' as u32) * 33 + b'c' as u32
    );
    ax_assert_ne!(
        htree_dir::calculate_hash(b"abc", htree_dir::Ext4DxRootInfo::DX_HASH_HALF_MD4, &seed),
        0
    );
    ax_assert_ne!(
        htree_dir::calculate_hash(b"abc", htree_dir::Ext4DxRootInfo::DX_HASH_TEA, &seed),
        0
    );
    ax_assert_eq!(htree_dir::calculate_hash(b"abc", 99, &seed), 0);

    let status = Ext4ExtentStatus {
        es_lblk: 1,
        es_len: 2,
        es_pblk: 3,
    };
    ax_assert_eq!(status.es_lblk + status.es_len + status.es_pblk, 6);
}

#[axtest::def_test]
fn rsext4_disk_format_and_journal_struct_rules_hold() {
    use rsext4::{
        BLOCK_SIZE_U32,
        disknode::{Ext4Extent, Ext4ExtentHeader, Ext4ExtentIdx, Ext4Inode},
        endian::DiskFormat,
        jbd2::jbdstruct::{
            CommitHeader, JBD2_BLOCKTYPE_COMMIT, JBD2_BLOCKTYPE_DESCRIPTOR, JBD2_CRC32C_CHKSUM,
            JBD2_MAGIC, JBD2_TAG_SIZE, Jbd2JournalBlockTail, Jbd2JournalRevokeHeadS,
            Jbd2JournalRevokeTail, JournalBlockTag3S, JournalBlockTagS, JournalHeaderS,
            JournalSuperBllockS,
        },
    };

    let header = Ext4ExtentHeader {
        eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
        eh_entries: 2,
        eh_max: 4,
        eh_depth: 1,
        eh_generation: 0x1122_3344,
    };
    let mut bytes = [0_u8; 12];
    header.to_disk_bytes(&mut bytes);
    let parsed = Ext4ExtentHeader::from_disk_bytes(&bytes);
    ax_assert_eq!(parsed.eh_magic, Ext4ExtentHeader::EXT4_EXT_MAGIC);
    ax_assert_eq!(parsed.eh_entries, 2);
    ax_assert_eq!(Ext4ExtentHeader::disk_size(), 12);

    let idx = Ext4ExtentIdx {
        ei_block: 9,
        ei_leaf_lo: 0x89ab_cdef,
        ei_leaf_hi: 0x1234,
        ei_unused: 0,
    };
    idx.to_disk_bytes(&mut bytes);
    let parsed_idx = Ext4ExtentIdx::from_disk_bytes(&bytes);
    ax_assert_eq!(parsed_idx.ei_block, 9);
    ax_assert_eq!(parsed_idx.ei_leaf_hi, 0x1234);

    let extent = Ext4Extent::new(4, 0x1234_5678_9abc, 15);
    extent.to_disk_bytes(&mut bytes);
    let parsed_extent = Ext4Extent::from_disk_bytes(&bytes);
    ax_assert_eq!(parsed_extent.ee_block, 4);
    ax_assert_eq!(parsed_extent.start_block(), 0x1234_5678_9abc);
    ax_assert_eq!(parsed_extent.len(), 15);

    let mut inode = Ext4Inode::empty_for_reuse(32);
    inode.set_uid(0x1234_5678);
    inode.set_gid(0x9abc_def0);
    inode.set_mode_full(Ext4Inode::S_IFREG | 0o644);
    inode.i_size_lo = 0xfeed_beef;
    inode.i_block[0] = 0x0102_0304;
    let mut inode_bytes = vec![0_u8; Ext4Inode::LARGE_INODE_SIZE as usize];
    inode.to_disk_bytes(&mut inode_bytes);
    let parsed_inode = Ext4Inode::from_disk_bytes(&inode_bytes);
    ax_assert_eq!(parsed_inode.uid(), 0x1234_5678);
    ax_assert_eq!(parsed_inode.gid(), 0x9abc_def0);
    ax_assert_eq!(parsed_inode.i_size_lo, 0xfeed_beef);
    ax_assert_eq!(parsed_inode.i_block[0], 0x0102_0304);
    ax_assert_eq!(
        Ext4Inode::disk_size(),
        Ext4Inode::GOOD_OLD_INODE_SIZE as usize
    );

    let journal_header = JournalHeaderS {
        h_magic: JBD2_MAGIC,
        h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
        h_sequence: 0x1122_3344,
    };
    let mut header_bytes = [0_u8; 12];
    journal_header.to_disk_bytes(&mut header_bytes);
    ax_assert_eq!(&header_bytes[0..4], &JBD2_MAGIC.to_be_bytes());
    let parsed_header = JournalHeaderS::from_disk_bytes(&header_bytes);
    ax_assert_eq!(parsed_header.h_magic, JBD2_MAGIC);
    ax_assert_eq!(parsed_header.h_sequence, 0x1122_3344);

    let mut journal_sb = JournalSuperBllockS {
        s_header: journal_header,
        s_blocksize: BLOCK_SIZE_U32,
        s_maxlen: 1024,
        s_first: 2,
        s_sequence: 0x0102_0304,
        s_start: 0x1122_3344,
        s_checksum_type: JBD2_CRC32C_CHKSUM,
        s_uuid: [0xaa; 16],
        s_users: [0x55; 16 * 48],
        ..Default::default()
    };
    let mut journal_bytes = [0_u8; 1024];
    journal_sb.to_disk_bytes(&mut journal_bytes);
    let parsed_sb = JournalSuperBllockS::from_disk_bytes(&journal_bytes);
    ax_assert_eq!(parsed_sb.s_blocksize, BLOCK_SIZE_U32);
    ax_assert_eq!(parsed_sb.s_sequence, 0x0102_0304);
    ax_assert_eq!(parsed_sb.s_users[0], 0x55);

    let checksum = rsext4::checksum::jbd2_superblock_csum32(&journal_sb);
    rsext4::checksum::jbd2_update_superblock_checksum(&mut journal_sb);
    ax_assert_eq!(journal_sb.s_checksum, checksum);
    journal_sb.s_checksum_type = 0;
    rsext4::checksum::jbd2_update_superblock_checksum(&mut journal_sb);
    ax_assert_eq!(journal_sb.s_checksum, 0);

    let tag = JournalBlockTagS {
        t_blocknr: 0xdead_beef,
        t_checksum: 0xabcd,
        t_flags: 1,
    };
    let mut tag_bytes = [0_u8; JBD2_TAG_SIZE];
    tag.to_disk_bytes(&mut tag_bytes);
    let parsed_tag = JournalBlockTagS::from_disk_bytes(&tag_bytes);
    ax_assert_eq!(parsed_tag.t_blocknr, tag.t_blocknr);
    ax_assert_eq!(parsed_tag.t_checksum, tag.t_checksum);

    let tag3 = JournalBlockTag3S {
        t_blocknr: 1,
        t_flags: 2,
        t_blocknr_high: 3,
        t_checksum: 4,
    };
    let mut tag3_bytes = [0_u8; 16];
    tag3.to_disk_bytes(&mut tag3_bytes);
    let parsed_tag3 = JournalBlockTag3S::from_disk_bytes(&tag3_bytes);
    ax_assert_eq!(parsed_tag3.t_blocknr_high, 3);
    ax_assert_eq!(parsed_tag3.t_checksum, 4);

    let tail = Jbd2JournalBlockTail {
        t_checksum: 0x1234_5678,
    };
    let mut tail_bytes = [0_u8; 4];
    tail.to_disk_bytes(&mut tail_bytes);
    ax_assert_eq!(
        Jbd2JournalBlockTail::from_disk_bytes(&tail_bytes).t_checksum,
        0x1234_5678
    );

    let revoke = Jbd2JournalRevokeHeadS {
        r_header: JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: 5,
            h_sequence: 7,
        },
        r_count: 16,
    };
    let mut revoke_bytes = [0_u8; 16];
    revoke.to_disk_bytes(&mut revoke_bytes);
    let parsed_revoke = Jbd2JournalRevokeHeadS::from_disk_bytes(&revoke_bytes);
    ax_assert_eq!(parsed_revoke.r_header.h_sequence, 7);
    ax_assert_eq!(parsed_revoke.r_count, 16);

    let revoke_tail = Jbd2JournalRevokeTail {
        r_checksum: 0xcafe_babe,
    };
    revoke_tail.to_disk_bytes(&mut tail_bytes);
    ax_assert_eq!(
        Jbd2JournalRevokeTail::from_disk_bytes(&tail_bytes).r_checksum,
        0xcafe_babe
    );

    let commit = CommitHeader {
        h_header: JournalHeaderS {
            h_magic: JBD2_MAGIC,
            h_blocktype: JBD2_BLOCKTYPE_COMMIT,
            h_sequence: 9,
        },
        h_chksum_type: JBD2_CRC32C_CHKSUM,
        h_chksum_size: 4,
        h_padding: [0, 0],
        h_chksum: [0x1111_2222; 8],
        h_commit_sec: 0x0102_0304_0506_0708,
        h_commit_nsec: 0xaabb_ccdd,
    };
    let mut commit_bytes = [0_u8; 64];
    commit.to_disk_bytes(&mut commit_bytes);
    let parsed_commit = CommitHeader::from_disk_bytes(&commit_bytes);
    ax_assert_eq!(parsed_commit.h_chksum_type, JBD2_CRC32C_CHKSUM);
    ax_assert_eq!(parsed_commit.h_commit_sec, 0x0102_0304_0506_0708);
    ax_assert_eq!(parsed_commit.h_commit_nsec, 0xaabb_ccdd);
}

#[axtest::def_test]
fn rsext4_checksum_blockgroup_and_api_helpers_hold() {
    use rsext4::{
        BLOCK_SIZE, Errno, Ext4Error,
        api::OpenFile,
        bitmap::bitmap_utils::set_bit,
        blockdev::{BlockBuffer, BlockDevice},
        blockgroup_description::{BlockGroupDescTable, BlockGroupDescTableMut, Ext4GroupDesc},
        bmalloc::{AbsoluteBN, BGIndex, InodeNumber, RelativeBN, RelativeInodeIndex},
        disknode::{Ext4Inode, Ext4Timestamp},
        endian::DiskFormat,
        entries::Ext4DirEntryTail,
        superblock::Ext4Superblock,
    };

    let mut block_buffer = BlockBuffer::new();
    ax_assert_eq!(block_buffer.len(), BLOCK_SIZE);
    ax_assert!(!block_buffer.is_empty());
    ax_assert!(block_buffer.as_slice().iter().all(|byte| *byte == 0));
    block_buffer.as_mut_slice()[0] = 7;
    ax_assert_eq!(block_buffer.as_slice()[0], 7);
    block_buffer.clear();
    ax_assert_eq!(block_buffer.as_slice()[0], 0);

    let group = BGIndex::new(3);
    let relative_block = RelativeBN::new(17);
    let absolute = group.absolute_block(relative_block, 1, 8192);
    let (decoded_group, decoded_block) = absolute.to_group(1, 8192).unwrap();
    ax_assert_eq!(decoded_group, group);
    ax_assert_eq!(decoded_block, relative_block);
    ax_assert_eq!(absolute.checked_add(5).unwrap().raw(), absolute.raw() + 5);
    ax_assert_eq!(
        AbsoluteBN::new(u64::from(u32::MAX) + 1)
            .to_u32()
            .unwrap_err()
            .code,
        Errno::EOVERFLOW
    );
    ax_assert_eq!(
        AbsoluteBN::new(0).to_group(1, 8192).unwrap_err().code,
        Errno::EINVAL
    );

    let inode_index = RelativeInodeIndex::new(123);
    let inode_num = group.inode_number(inode_index, 2048).unwrap();
    let (decoded_inode_group, decoded_inode_index) = inode_num.to_group(2048).unwrap();
    ax_assert_eq!(decoded_inode_group, group);
    ax_assert_eq!(decoded_inode_index, inode_index);
    ax_assert_eq!(InodeNumber::new(0).unwrap_err().code, Errno::EINVAL);
    ax_assert_eq!(inode_num.as_u64(), u64::from(inode_num.raw()));

    let descs = [
        Ext4GroupDesc {
            bg_free_blocks_count_lo: 20,
            bg_free_inodes_count_lo: 4,
            bg_used_dirs_count_lo: 1,
            ..Default::default()
        },
        Ext4GroupDesc {
            bg_free_blocks_count_lo: 40,
            bg_free_inodes_count_lo: 0,
            bg_used_dirs_count_lo: 2,
            bg_flags: Ext4GroupDesc::EXT4_BG_BLOCK_UNINIT | Ext4GroupDesc::EXT4_BG_INODE_UNINIT,
            ..Default::default()
        },
    ];
    let mut table_bytes = vec![0_u8; 2 * Ext4GroupDesc::EXT4_DESC_SIZE_64BIT];
    descs[0].to_disk_bytes(&mut table_bytes[0..64]);
    descs[1].to_disk_bytes(&mut table_bytes[64..128]);
    let table = BlockGroupDescTable::new(&table_bytes, 64, 2);
    ax_assert_eq!(table.group_count(), 2);
    ax_assert_eq!(table.desc_size(), 64);
    ax_assert_eq!(table.total_free_blocks(), 60);
    ax_assert_eq!(table.total_free_inodes(), 4);
    ax_assert_eq!(table.total_used_dirs(), 3);
    ax_assert_eq!(table.find_group_with_free_blocks(10).unwrap().raw(), 0);
    ax_assert_eq!(table.find_group_with_free_blocks(30), None);
    ax_assert_eq!(table.find_group_with_free_inodes().unwrap().raw(), 0);
    ax_assert_eq!(table.iter().count(), 2);
    ax_assert!(table.get_desc(BGIndex::new(2)).is_none());

    let mut mutable_table = BlockGroupDescTableMut::new(&mut table_bytes, 64, 2);
    ax_assert!(mutable_table.update_free_blocks(BGIndex::new(0), 0x1_0002));
    ax_assert!(mutable_table.update_free_inodes(BGIndex::new(0), 0x2_0003));
    ax_assert!(mutable_table.update_used_dirs(BGIndex::new(0), 7));
    ax_assert!(mutable_table.increment_used_dirs(BGIndex::new(0)));
    ax_assert!(mutable_table.decrement_used_dirs(BGIndex::new(0)));
    ax_assert!(mutable_table.set_flags(BGIndex::new(0), Ext4GroupDesc::EXT4_BG_INODE_ZEROED));
    ax_assert!(mutable_table.clear_flags(BGIndex::new(0), Ext4GroupDesc::EXT4_BG_INODE_ZEROED));
    ax_assert!(!mutable_table.update_free_blocks(BGIndex::new(3), 1));

    let table = BlockGroupDescTable::new(&table_bytes, 64, 2);
    let desc = table.get_desc(BGIndex::new(0)).unwrap();
    ax_assert_eq!(desc.free_blocks_count(), 0x1_0002);
    ax_assert_eq!(desc.free_inodes_count(), 0x2_0003);
    ax_assert_eq!(desc.used_dirs_count(), 7);

    let mut superblock = Ext4Superblock {
        s_magic: Ext4Superblock::EXT4_SUPER_MAGIC,
        s_log_block_size: 2,
        s_blocks_per_group: 8192,
        s_clusters_per_group: 8192,
        s_inodes_per_group: 8192,
        s_inode_size: Ext4Inode::LARGE_INODE_SIZE,
        s_uuid: [0x33; 16],
        s_feature_ro_compat: Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM,
        ..Default::default()
    };
    let bitmap = [0xaa_u8; 1024];
    let block_csum = rsext4::checksum::ext4_block_bitmap_csum32(&superblock, &bitmap);
    let inode_csum = rsext4::checksum::ext4_inode_bitmap_csum32(&superblock, &bitmap);
    ax_assert_ne!(block_csum, 0);
    ax_assert_ne!(inode_csum, 0);
    ax_assert_eq!(
        rsext4::checksum::ext4_metadata_block_csum32(&superblock, b"metadata"),
        rsext4::checksum::ext4_metadata_csum32(
            rsext4::crc32c::ext4_crc32c_seed_from_superblock(&superblock),
            &[b"metadata"]
        )
    );

    let mut block = vec![0_u8; BLOCK_SIZE];
    block[BLOCK_SIZE - 5] = Ext4DirEntryTail::RESERVED_FT;
    rsext4::checksum::update_ext4_dirblock_csum32(&superblock, 2, 3, &mut block);
    ax_assert!(rsext4::checksum::verify_ext4_dirblock_checksum(
        &superblock,
        2,
        3,
        &block
    ));
    block[0] ^= 1;
    ax_assert!(!rsext4::checksum::verify_ext4_dirblock_checksum(
        &superblock,
        2,
        3,
        &block
    ));
    ax_assert!(rsext4::checksum::verify_ext4_dirblock_checksum(
        &Ext4Superblock::default(),
        2,
        3,
        &[]
    ));

    let mut short = [0_u8; 8];
    rsext4::checksum::ext4_update_dirblock_tail_checksum(&superblock, 2, 3, &mut short, 0);
    ax_assert_eq!(short, [0; 8]);

    let mut inode = Ext4Inode::empty_for_reuse(32);
    inode.i_generation = 0x7788_99aa;
    let checksum = rsext4::checksum::ext4_inode_csum32(
        &superblock,
        inode_num,
        inode.i_generation,
        &inode,
        256,
    );
    rsext4::checksum::ext4_update_inode_checksum(
        &superblock,
        inode_num,
        inode.i_generation,
        &mut inode,
        256,
    );
    ax_assert_eq!(inode.l_i_checksum_lo, (checksum & 0xffff) as u16);
    ax_assert_eq!(inode.i_checksum_hi, (checksum >> 16) as u16);

    let old_checksum = superblock.s_checksum;
    rsext4::checksum::ext4_update_superblock_checksum(&mut superblock);
    ax_assert_ne!(superblock.s_checksum, old_checksum);

    let mut file = OpenFile {
        inode_num,
        path: "/alpha".to_string(),
        inode,
        offset: 0,
    };
    rsext4::lseek(&mut file, 1234).unwrap();
    ax_assert_eq!(file.offset, 1234);

    struct ReadonlyDevice;
    impl BlockDevice for ReadonlyDevice {
        fn write(
            &mut self,
            _buffer: &[u8],
            _block_id: AbsoluteBN,
            _count: u32,
        ) -> rsext4::Ext4Result<()> {
            Err(Ext4Error::read_only())
        }

        fn read(
            &mut self,
            buffer: &mut [u8],
            _block_id: AbsoluteBN,
            _count: u32,
        ) -> rsext4::Ext4Result<()> {
            buffer.fill(0x5a);
            Ok(())
        }

        fn open(&mut self) -> rsext4::Ext4Result<()> {
            Ok(())
        }

        fn close(&mut self) -> rsext4::Ext4Result<()> {
            Ok(())
        }

        fn total_blocks(&self) -> u64 {
            99
        }

        fn current_time(&self) -> rsext4::Ext4Result<Ext4Timestamp> {
            Ok(Ext4Timestamp::new(12, 34))
        }

        fn is_readonly(&self) -> bool {
            true
        }
    }

    let mut dev = ReadonlyDevice;
    ax_assert_eq!(dev.block_size(), 512);
    ax_assert!(dev.is_open());
    ax_assert!(dev.is_readonly());
    ax_assert_eq!(dev.total_blocks(), 99);
    dev.flush().unwrap();
    let mut buf = [0_u8; 4];
    dev.read(&mut buf, AbsoluteBN::new(0), 1).unwrap();
    ax_assert_eq!(buf, [0x5a; 4]);
    ax_assert_eq!(
        dev.write(&buf, AbsoluteBN::new(0), 1).unwrap_err().code,
        Errno::EROFS
    );
    ax_assert_eq!(dev.current_time().unwrap(), Ext4Timestamp::new(12, 34));

    let mut bitmap = [0_u8; 2];
    ax_assert!(set_bit(&mut bitmap, 15));
    ax_assert_eq!(bitmap[1], 0x80);
}

#[axtest::def_test]
fn rsext4_journal_device_overlay_rules_hold() {
    use core::cell::Cell;

    use rsext4::{
        BLOCK_SIZE, BlockDevice, Ext4Result, Jbd2Dev, bmalloc::AbsoluteBN, disknode::Ext4Timestamp,
        jbd2::jbdstruct::JournalSuperBllockS,
    };

    struct JournalMemoryDevice {
        blocks: Vec<u8>,
        now: Cell<i64>,
        readonly: bool,
    }

    impl JournalMemoryDevice {
        fn new(block_count: usize) -> Self {
            Self {
                blocks: vec![0; block_count * BLOCK_SIZE],
                now: Cell::new(1_900_000_000),
                readonly: false,
            }
        }

        fn new_readonly(block_count: usize) -> Self {
            Self {
                blocks: vec![0; block_count * BLOCK_SIZE],
                now: Cell::new(1_900_000_000),
                readonly: true,
            }
        }
    }

    impl BlockDevice for JournalMemoryDevice {
        fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            if buffer.len() < required {
                return Err(rsext4::Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + required;
            if end > self.blocks.len() {
                return Err(rsext4::Ext4Error::block_out_of_range(
                    block_id.raw().min(u64::from(u32::MAX)) as u32,
                    self.total_blocks(),
                ));
            }
            self.blocks[start..end].copy_from_slice(&buffer[..required]);
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            if buffer.len() < required {
                return Err(rsext4::Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + required;
            if end > self.blocks.len() {
                return Err(rsext4::Ext4Error::block_out_of_range(
                    block_id.raw().min(u64::from(u32::MAX)) as u32,
                    self.total_blocks(),
                ));
            }
            buffer[..required].copy_from_slice(&self.blocks[start..end]);
            Ok(())
        }

        fn open(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn close(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn total_blocks(&self) -> u64 {
            (self.blocks.len() / BLOCK_SIZE) as u64
        }

        fn block_size(&self) -> u32 {
            BLOCK_SIZE as u32
        }

        fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
            let sec = self.now.get();
            self.now.set(sec + 1);
            Ok(Ext4Timestamp::new(sec, 0))
        }

        fn is_readonly(&self) -> bool {
            self.readonly
        }
    }

    let mut dev = Jbd2Dev::initial_jbd2dev(0, JournalMemoryDevice::new(32), false);
    ax_assert!(!dev.is_use_journal());
    ax_assert_eq!(dev.journal_sequence(), None);
    dev.journal_replay();
    ax_assert_eq!(dev.total_blocks(), 32);
    ax_assert_eq!(dev.block_size(), BLOCK_SIZE as u32);
    ax_assert_eq!(
        dev.current_time().unwrap(),
        Ext4Timestamp::new(1_900_000_000, 0)
    );

    dev.read_block(AbsoluteBN::new(1)).unwrap();
    dev.buffer_mut()[0] = 0x11;
    dev.write_block(AbsoluteBN::new(1), false).unwrap();
    dev.read_block(AbsoluteBN::new(1)).unwrap();
    ax_assert_eq!(dev.buffer()[0], 0x11);
    dev.read_block(AbsoluteBN::new(3)).unwrap();
    let mut direct = vec![0_u8; BLOCK_SIZE];
    direct[0] = 0x66;
    dev.write_blocks(&direct, AbsoluteBN::new(3), 1, false)
        .unwrap();
    ax_assert_eq!(dev.buffer()[0], 0x66);

    dev.set_journal_use(true);
    dev.journal_replay();
    let journal_superblock = JournalSuperBllockS {
        s_sequence: 7,
        s_maxlen: 8,
        ..Default::default()
    };
    dev.set_journal_superblock(journal_superblock, AbsoluteBN::new(16));
    ax_assert!(dev.is_use_journal());
    ax_assert_eq!(dev.journal_sequence(), Some(7));

    dev.read_block(AbsoluteBN::new(2)).unwrap();
    dev.buffer_mut()[0] = 0x22;
    dev.write_block(AbsoluteBN::new(2), true).unwrap();
    dev.read_block(AbsoluteBN::new(2)).unwrap();
    ax_assert_eq!(dev.buffer()[0], 0x22);

    let mut observed = vec![0_u8; BLOCK_SIZE * 2];
    dev.read_blocks(&mut observed, AbsoluteBN::new(2), 2)
        .unwrap();
    ax_assert_eq!(observed[0], 0x22);

    let mut replacement = vec![0_u8; BLOCK_SIZE * 2];
    replacement[0] = 0x33;
    replacement[BLOCK_SIZE] = 0x44;
    dev.write_blocks(&replacement, AbsoluteBN::new(2), 2, true)
        .unwrap();
    dev.read_blocks(&mut observed, AbsoluteBN::new(2), 2)
        .unwrap();
    ax_assert_eq!(observed[0], 0x33);
    ax_assert_eq!(observed[BLOCK_SIZE], 0x44);
    let mut uncached_pending = vec![0_u8; BLOCK_SIZE];
    uncached_pending[0] = 0x77;
    dev.write_blocks(&uncached_pending, AbsoluteBN::new(6), 1, true)
        .unwrap();
    dev.read_block(AbsoluteBN::new(6)).unwrap();
    ax_assert_eq!(dev.buffer()[0], 0x77);
    ax_assert!(
        dev.write_blocks(&replacement[..8], AbsoluteBN::new(4), 1, true)
            .is_err()
    );

    dev.set_journal_use(false);
    ax_assert!(
        dev.write_blocks(&direct[..8], AbsoluteBN::new(4), 1, false)
            .is_err()
    );
    dev.cantflush().unwrap();
    let inner = dev.into_inner();
    ax_assert_eq!(inner.total_blocks(), 32);

    let mut readonly_dev = Jbd2Dev::initial_jbd2dev(0, JournalMemoryDevice::new_readonly(8), false);
    readonly_dev.read_block(AbsoluteBN::new(1)).unwrap();
    readonly_dev.buffer_mut()[0] = 0xaa;
    ax_assert!(readonly_dev.write_block(AbsoluteBN::new(1), false).is_err());
    ax_assert!(
        readonly_dev
            .write_blocks(&replacement, AbsoluteBN::new(1), 1, false)
            .is_err()
    );
    ax_assert!(
        readonly_dev
            .read_blocks(&mut observed[..8], AbsoluteBN::new(1), 1)
            .is_err()
    );
}

fn write_dirent(slot: &mut [u8], inode: u32, rec_len: u16, file_type: u8, name: &[u8]) {
    slot.fill(0);
    slot[0..4].copy_from_slice(&inode.to_le_bytes());
    slot[4..6].copy_from_slice(&rec_len.to_le_bytes());
    slot[6] = name.len() as u8;
    slot[7] = file_type;
    slot[8..8 + name.len()].copy_from_slice(name);
}

#[axtest::def_test]
fn rsext4_extent_tree_parse_store_and_hash_tree_rules_hold() {
    use rsext4::{
        bmalloc::AbsoluteBN,
        disknode::{Ext4Extent, Ext4ExtentHeader, Ext4ExtentIdx, Ext4Inode},
        endian::DiskFormat,
        entries::{Ext4DirEntry2, Ext4DirEntryInfo, Ext4DxEntry, Ext4DxRootInfo},
        extents_tree::{ExtentNode, ExtentRun, ExtentTree},
        hashtree::{
            Ext4InodeHashTreeExt, HashTreeError, HashTreeManager, HashTreeNode,
            HashTreeSearchResult,
        },
    };

    let mut leaf_bytes = [0_u8; 60];
    let leaf_header = Ext4ExtentHeader {
        eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
        eh_entries: 2,
        eh_max: 4,
        eh_depth: 0,
        eh_generation: 1,
    };
    leaf_header.to_disk_bytes(&mut leaf_bytes[0..12]);
    Ext4Extent::new(8, 200, 2).to_disk_bytes(&mut leaf_bytes[12..24]);
    Ext4Extent::new(3, 100, 4).to_disk_bytes(&mut leaf_bytes[24..36]);

    let leaf = ExtentTree::parse_node(&leaf_bytes).unwrap();
    ax_assert!(leaf.is_leaf());
    ax_assert_eq!(leaf.header().eh_entries, 2);
    match &leaf {
        ExtentNode::Leaf { entries, .. } => {
            ax_assert_eq!(entries[0].ee_block, 3);
            ax_assert_eq!(entries[1].ee_block, 8);
        }
        ExtentNode::Index { .. } => panic!("expected leaf extent node"),
    }

    let mut index_bytes = [0_u8; 60];
    let index_header = Ext4ExtentHeader {
        eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
        eh_entries: 2,
        eh_max: 4,
        eh_depth: 1,
        eh_generation: 2,
    };
    index_header.to_disk_bytes(&mut index_bytes[0..12]);
    Ext4ExtentIdx {
        ei_block: 9,
        ei_leaf_lo: 0x2222_3333,
        ei_leaf_hi: 0x1111,
        ei_unused: 0,
    }
    .to_disk_bytes(&mut index_bytes[12..24]);
    Ext4ExtentIdx {
        ei_block: 1,
        ei_leaf_lo: 0x5555_6666,
        ei_leaf_hi: 0x4444,
        ei_unused: 0,
    }
    .to_disk_bytes(&mut index_bytes[24..36]);
    let index = ExtentTree::parse_node(&index_bytes).unwrap();
    ax_assert!(!index.is_leaf());
    match &index {
        ExtentNode::Index { entries, .. } => {
            ax_assert_eq!(entries[0].ei_block, 1);
            ax_assert_eq!(entries[1].ei_block, 9);
        }
        ExtentNode::Leaf { .. } => panic!("expected index extent node"),
    }

    let mut bad = leaf_bytes;
    bad[0] = 0;
    ax_assert!(ExtentTree::parse_node(&bad).is_none());
    let mut overflow = leaf_bytes;
    overflow[2..4].copy_from_slice(&5_u16.to_le_bytes());
    overflow[4..6].copy_from_slice(&4_u16.to_le_bytes());
    ax_assert!(ExtentTree::parse_node(&overflow).is_none());
    ax_assert!(ExtentTree::parse_node(&leaf_bytes[..20]).is_none());

    let mut inode = Ext4Inode::empty_for_reuse(32);
    inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    {
        let mut tree = ExtentTree::new(&mut inode);
        tree.store_root_to_inode(&leaf);
        let mut loaded = tree.load_root_from_inode().unwrap();
        ax_assert!(loaded.is_leaf());
        let header = loaded.header_mut();
        header.eh_generation = 99;
        ax_assert_eq!(loaded.header().eh_generation, 99);
    }
    ax_assert!(inode.have_extend_header_and_use_extend());

    let run = ExtentRun {
        logical_start: 3,
        physical_start: AbsoluteBN::new(100),
        len: 4,
    };
    ax_assert_eq!(run.logical_start, 3);
    ax_assert_eq!(run.physical_start.raw(), 100);
    ax_assert_eq!(run.len, 4);

    let mut inode = Ext4Inode::default();
    ax_assert!(!inode.is_htree_indexed());
    ax_assert_eq!(inode.get_htree_root_info(), None);
    inode.i_flags |= Ext4Inode::EXT4_INDEX_FL;
    ax_assert!(inode.is_htree_indexed());
    let (_, indirect_levels) = inode.get_htree_root_info().unwrap();
    ax_assert_eq!(indirect_levels, 0);

    let errors = [
        (HashTreeError::InvalidHashTree, "Invalid hash tree format"),
        (
            HashTreeError::UnsupportedHashVersion,
            "Unsupported hash version",
        ),
        (HashTreeError::CorruptedHashTree, "Corrupted hash tree"),
        (HashTreeError::BlockOutOfRange, "Block number out of range"),
        (HashTreeError::BufferTooSmall, "Buffer too small"),
        (HashTreeError::EntryNotFound, "Entry not found"),
    ];
    for (error, text) in errors {
        ax_assert_eq!(error.to_string(), text);
    }

    let manager = HashTreeManager::new([1, 2, 3, 4], Ext4DxRootInfo::DX_HASH_LEGACY, 0);
    let _ = manager;
    let root_node = HashTreeNode::Root {
        hash_version: 1,
        indirect_levels: 0,
        entries: vec![Ext4DxEntry { hash: 7, block: 2 }],
    };
    let internal_node = HashTreeNode::Internal {
        entries: vec![Ext4DxEntry { hash: 9, block: 3 }],
        level: 1,
    };
    let leaf_node = HashTreeNode::Leaf {
        block_num: AbsoluteBN::new(4),
        entries: vec![Ext4DirEntryInfo {
            inode: 5,
            file_type: Ext4DirEntry2::EXT4_FT_REG_FILE,
            name: b"leaf",
        }],
    };
    match root_node {
        HashTreeNode::Root {
            hash_version,
            indirect_levels,
            entries,
        } => {
            ax_assert_eq!(hash_version, 1);
            ax_assert_eq!(indirect_levels, 0);
            ax_assert_eq!(entries[0].block, 2);
        }
        _ => panic!("expected root hash tree node"),
    }
    match internal_node {
        HashTreeNode::Internal { entries, level } => {
            ax_assert_eq!(entries[0].hash, 9);
            ax_assert_eq!(level, 1);
        }
        _ => panic!("expected internal hash tree node"),
    }
    match leaf_node {
        HashTreeNode::Leaf { block_num, entries } => {
            ax_assert_eq!(block_num.raw(), 4);
            ax_assert_eq!(entries[0].name, b"leaf");
        }
        _ => panic!("expected leaf hash tree node"),
    }

    let result = HashTreeSearchResult {
        entry: Ext4DirEntryInfo {
            inode: 8,
            file_type: Ext4DirEntry2::EXT4_FT_DIR,
            name: b"dir",
        },
        block_num: AbsoluteBN::new(12),
        offset: 16,
    };
    ax_assert_eq!(result.entry.name, b"dir");
    ax_assert_eq!(result.block_num.raw(), 12);
    ax_assert_eq!(result.offset, 16);
}

#[axtest::def_test]
fn rsext4_tool_layout_and_blockgroup_disk_rules_hold() {
    use rsext4::{
        RESERVED_GDT_BLOCKS,
        blockgroup_description::Ext4GroupDesc,
        bmalloc::{AbsoluteBN, BGIndex, InodeNumber, RelativeBN, RelativeInodeIndex},
        endian::DiskFormat,
        superblock::Ext4Superblock,
        tool,
    };

    let uuid = tool::generate_uuid();
    let uuid_bytes = tool::generate_uuid_8();
    ax_assert_ne!(uuid.0, [0; 4]);
    ax_assert_ne!(uuid_bytes, [0; 16]);
    ax_assert!(tool::is_numbers_power(1, 3));
    ax_assert!(tool::is_numbers_power(27, 3));
    ax_assert!(!tool::is_numbers_power(28, 3));
    ax_assert!(tool::need_redundant_backup(0));
    ax_assert!(tool::need_redundant_backup(1));
    ax_assert!(tool::need_redundant_backup(3 * 3 * 3));
    ax_assert!(tool::need_redundant_backup(5 * 5));
    ax_assert!(tool::need_redundant_backup(7 * 7));
    ax_assert!(!tool::need_redundant_backup(11));

    let mut superblock = Ext4Superblock {
        s_feature_ro_compat: Ext4Superblock::EXT4_FEATURE_RO_COMPAT_SPARSE_SUPER,
        ..Default::default()
    };
    let layout0 = tool::cloc_group_layout(0, &superblock, 8192, 64, 3, 4, 5, RESERVED_GDT_BLOCKS);
    ax_assert_eq!(layout0.group_start_block, 0);
    ax_assert_eq!(layout0.group_blcok_bitmap_startblocks, 3);
    ax_assert_eq!(layout0.group_inode_bitmap_startblocks, 4);
    ax_assert_eq!(layout0.group_inode_table_startblocks, 5);

    let sparse = tool::cloc_group_layout(3, &superblock, 8192, 64, 3, 4, 5, 2);
    ax_assert_eq!(sparse.group_start_block, 3 * 8192);
    ax_assert_eq!(sparse.group_blcok_bitmap_startblocks, 3 * 8192 + 1 + 2);
    ax_assert_eq!(sparse.group_inode_bitmap_startblocks, 3 * 8192 + 1 + 2 + 1);
    ax_assert_eq!(sparse.metadata_blocks_in_group, 1 + 2 + 1 + 1 + 64);

    superblock.s_feature_ro_compat = 0;
    let dense = tool::cloc_group_layout(3, &superblock, 8192, 64, 3, 4, 5, 2);
    ax_assert_eq!(dense.group_blcok_bitmap_startblocks, 3 * 8192);
    ax_assert_eq!(dense.group_inode_bitmap_startblocks, 3 * 8192 + 1);
    ax_assert_eq!(dense.group_inode_table_startblocks, 3 * 8192 + 2);
    ax_assert_eq!(dense.metadata_blocks_in_group, 1 + 1 + 64);

    let desc = Ext4GroupDesc {
        bg_block_bitmap_lo: 1,
        bg_inode_bitmap_lo: 2,
        bg_inode_table_lo: 3,
        bg_free_blocks_count_lo: 4,
        bg_free_inodes_count_lo: 5,
        bg_used_dirs_count_lo: 6,
        bg_flags: Ext4GroupDesc::EXT4_BG_INODE_ZEROED,
        bg_exclude_bitmap_lo: 7,
        bg_block_bitmap_csum_lo: 8,
        bg_inode_bitmap_csum_lo: 9,
        bg_itable_unused_lo: 10,
        bg_checksum: 11,
        bg_block_bitmap_hi: 12,
        bg_inode_bitmap_hi: 13,
        bg_inode_table_hi: 14,
        bg_free_blocks_count_hi: 15,
        bg_free_inodes_count_hi: 16,
        bg_used_dirs_count_hi: 17,
        bg_itable_unused_hi: 18,
        bg_exclude_bitmap_hi: 19,
        bg_block_bitmap_csum_hi: 20,
        bg_inode_bitmap_csum_hi: 21,
        bg_reserved: 22,
    };
    let mut old_bytes = [0_u8; 32];
    desc.to_disk_bytes(&mut old_bytes);
    let old_desc = Ext4GroupDesc::from_disk_bytes(&old_bytes);
    ax_assert_eq!(old_desc.block_bitmap(), 1);
    ax_assert_eq!(old_desc.inode_bitmap(), 2);
    ax_assert_eq!(old_desc.inode_table(), 3);
    ax_assert_eq!(old_desc.bg_block_bitmap_hi, 0);

    let mut new_bytes = [0_u8; 64];
    desc.to_disk_bytes(&mut new_bytes);
    let new_desc = Ext4GroupDesc::from_disk_bytes(&new_bytes);
    ax_assert_eq!(new_desc.block_bitmap(), (12_u64 << 32) | 1);
    ax_assert_eq!(new_desc.inode_bitmap(), (13_u64 << 32) | 2);
    ax_assert_eq!(new_desc.inode_table(), (14_u64 << 32) | 3);
    ax_assert_eq!(new_desc.free_blocks_count(), (15 << 16) | 4);

    let checksum = rsext4::checksum::ext4_group_desc_csum16(&superblock, 3, &new_bytes);
    ax_assert_ne!(checksum, 0);

    ax_assert_eq!(BGIndex::new(2).as_usize().unwrap(), 2);
    ax_assert_eq!(RelativeBN::new(7).as_usize().unwrap(), 7);
    ax_assert_eq!(RelativeInodeIndex::new(8).as_usize().unwrap(), 8);
    ax_assert_eq!(AbsoluteBN::from(9_u32).raw(), 9);
    ax_assert_eq!(AbsoluteBN::new(10).checked_add_usize(5).unwrap().raw(), 15);
    ax_assert_eq!(InodeNumber::from_u64(123).unwrap().raw(), 123);
    ax_assert_eq!(InodeNumber::new(123).unwrap().as_usize().unwrap(), 123);
    ax_assert_eq!(BGIndex::new(2).to_string(), "2");
    ax_assert_eq!(AbsoluteBN::new(10).to_string(), "10");
    ax_assert_eq!(RelativeBN::new(7).to_string(), "7");
    ax_assert_eq!(InodeNumber::new(123).unwrap().to_string(), "123");
    ax_assert_eq!(RelativeInodeIndex::new(8).to_string(), "8");
}

#[axtest::def_test]
fn rsext4_path_and_bitmap_rules_hold() {
    use rsext4::{
        bitmap::{BitmapError, BlockBitmap, InodeBitmap},
        dir::split_paren_child_and_translatevalid,
    };

    ax_assert_eq!(split_paren_child_and_translatevalid(""), "");
    ax_assert_eq!(split_paren_child_and_translatevalid("/"), "/");
    ax_assert_eq!(split_paren_child_and_translatevalid("///"), "/");
    ax_assert_eq!(
        split_paren_child_and_translatevalid("//alpha///beta//"),
        "/alpha/beta"
    );
    ax_assert_eq!(
        split_paren_child_and_translatevalid("alpha//beta///gamma"),
        "alpha/beta/gamma"
    );

    let bitmap_errors = [
        (BitmapError::IndexOutOfRange, "bitmap index out of range"),
        (
            BitmapError::AlreadyAllocated,
            "bitmap entry is already allocated",
        ),
        (BitmapError::AlreadyFree, "bitmap entry is already free"),
    ];
    for (error, text) in bitmap_errors {
        ax_assert_eq!(error.to_string(), text);
    }

    let mut block_data = vec![0_u8; 2];
    {
        let mut bitmap = BlockBitmap::new(&mut block_data, 12);
        ax_assert_eq!(bitmap.is_allocated(0), Some(false));
        ax_assert_eq!(bitmap.is_free(0), Some(true));
        ax_assert_eq!(bitmap.is_allocated(12), None);
        ax_assert_eq!(bitmap.find_first_free(), Some(0));
        ax_assert_eq!(bitmap.find_contiguous_free(0), None);
        ax_assert_eq!(bitmap.find_contiguous_free(3), Some(0));
        ax_assert_eq!(bitmap.count_free(), 12);
        ax_assert_eq!(bitmap.count_allocated(), 0);

        bitmap.allocate(0).unwrap();
        bitmap.allocate(2).unwrap();
        ax_assert_eq!(
            bitmap.allocate(2).unwrap_err(),
            BitmapError::AlreadyAllocated
        );
        ax_assert_eq!(bitmap.is_allocated(0), Some(true));
        ax_assert_eq!(bitmap.find_first_free(), Some(1));
        ax_assert_eq!(bitmap.find_contiguous_free(2), Some(3));
        ax_assert_eq!(bitmap.count_allocated(), 2);

        bitmap.allocate_range(4, 3).unwrap();
        ax_assert_eq!(bitmap.is_allocated(5), Some(true));
        ax_assert_eq!(
            bitmap.allocate_range(5, 2).unwrap_err(),
            BitmapError::AlreadyAllocated
        );
        bitmap.free_range(4, 3).unwrap();
        ax_assert_eq!(bitmap.free(4).unwrap_err(), BitmapError::AlreadyFree);
        ax_assert_eq!(bitmap.free(20).unwrap_err(), BitmapError::IndexOutOfRange);
        ax_assert_eq!(
            bitmap.allocate(20).unwrap_err(),
            BitmapError::IndexOutOfRange
        );
    }
    ax_assert_eq!(block_data[0] & 0b0000_0101, 0b0000_0101);

    let mut inode_data = vec![0xFF_u8, 0_u8];
    {
        let mut bitmap = InodeBitmap::new(&mut inode_data, 12);
        ax_assert_eq!(bitmap.find_first_free(), Some(8));
        ax_assert_eq!(bitmap.count_allocated(), 8);
        ax_assert_eq!(bitmap.count_free(), 4);
        ax_assert_eq!(bitmap.is_allocated(12), None);
        bitmap.allocate(8).unwrap();
        ax_assert_eq!(
            bitmap.allocate(8).unwrap_err(),
            BitmapError::AlreadyAllocated
        );
        ax_assert_eq!(bitmap.is_free(8), Some(false));
        bitmap.free(8).unwrap();
        ax_assert_eq!(bitmap.free(8).unwrap_err(), BitmapError::AlreadyFree);
        ax_assert_eq!(
            bitmap.allocate(20).unwrap_err(),
            BitmapError::IndexOutOfRange
        );
        ax_assert_eq!(bitmap.free(20).unwrap_err(), BitmapError::IndexOutOfRange);
    }
    ax_assert_eq!(inode_data[1] & 1, 0);
}

#[axtest::def_test]
fn rsext4_bmalloc_allocator_rules_hold() {
    use rsext4::{
        Errno,
        blockgroup_description::Ext4GroupDesc,
        bmalloc::{
            AbsoluteBN, BGIndex, BlockAllocator, InodeAllocator, InodeNumber, RelativeBN,
            RelativeInodeIndex,
        },
        superblock::Ext4Superblock,
    };

    let block_superblock = Ext4Superblock {
        s_blocks_per_group: 16,
        s_first_data_block: 1,
        ..Default::default()
    };
    let block_allocator = BlockAllocator::new(&block_superblock);
    let group = BGIndex::new(2);
    let mut block_bitmap = vec![0b0000_1111_u8, 0_u8];
    let desc = Ext4GroupDesc {
        bg_free_blocks_count_lo: 12,
        ..Default::default()
    };
    let alloc = block_allocator
        .alloc_block_in_group(&mut block_bitmap, group, &desc)
        .unwrap();
    ax_assert_eq!(alloc.group_idx, group);
    ax_assert_eq!(alloc.block_in_group, RelativeBN::new(4));
    ax_assert_eq!(alloc.global_block, AbsoluteBN::new(37));
    ax_assert_eq!(
        block_allocator.global_to_group(alloc.global_block).unwrap(),
        (group, RelativeBN::new(4))
    );

    let no_space_desc = Ext4GroupDesc::default();
    ax_assert_eq!(
        block_allocator
            .alloc_block_in_group(&mut block_bitmap, group, &no_space_desc)
            .unwrap_err()
            .code,
        Errno::ENOSPC
    );

    let range = block_allocator
        .alloc_contiguous_blocks(&mut block_bitmap, group, 3)
        .unwrap();
    ax_assert_eq!(range.block_in_group, RelativeBN::new(5));
    ax_assert_eq!(range.global_block, AbsoluteBN::new(38));
    block_allocator
        .free_blocks(&mut block_bitmap, range.block_in_group, 3)
        .unwrap();
    block_allocator
        .free_block(&mut block_bitmap, alloc.block_in_group)
        .unwrap();
    ax_assert_eq!(
        block_allocator
            .alloc_contiguous_blocks(&mut block_bitmap, group, 0)
            .unwrap_err()
            .code,
        Errno::EINVAL
    );
    ax_assert_eq!(
        block_allocator
            .global_to_group(AbsoluteBN::new(0))
            .unwrap_err()
            .code,
        Errno::EINVAL
    );

    let inode_superblock = Ext4Superblock {
        s_inodes_per_group: 16,
        s_first_ino: 5,
        ..Default::default()
    };
    let inode_allocator = InodeAllocator::new(&inode_superblock);
    let mut inode_bitmap = vec![0b0001_1111_u8, 0_u8];
    let inode_desc = Ext4GroupDesc {
        bg_free_inodes_count_lo: 11,
        ..Default::default()
    };
    let inode_alloc = inode_allocator
        .alloc_inode_in_group(&mut inode_bitmap, BGIndex::new(1), &inode_desc)
        .unwrap();
    ax_assert_eq!(inode_alloc.group_idx, BGIndex::new(1));
    ax_assert_eq!(inode_alloc.inode_in_group, RelativeInodeIndex::new(5));
    ax_assert_eq!(inode_alloc.global_inode, InodeNumber::new(22).unwrap());
    ax_assert_eq!(
        inode_allocator
            .global_to_group(inode_alloc.global_inode)
            .unwrap(),
        (BGIndex::new(1), RelativeInodeIndex::new(5))
    );
    ax_assert_eq!(
        inode_allocator
            .inode_is_free(&mut inode_bitmap, inode_alloc.inode_in_group)
            .unwrap(),
        true
    );
    inode_allocator
        .free_inode(&mut inode_bitmap, inode_alloc.inode_in_group)
        .unwrap();
    ax_assert_eq!(
        inode_allocator
            .inode_is_free(&mut inode_bitmap, inode_alloc.inode_in_group)
            .unwrap(),
        false
    );
    ax_assert_eq!(
        inode_allocator
            .alloc_inode_in_group(
                &mut inode_bitmap,
                BGIndex::new(0),
                &Ext4GroupDesc::default()
            )
            .unwrap_err()
            .code,
        Errno::ENOSPC
    );
    ax_assert_eq!(InodeNumber::new(0).unwrap_err().code, Errno::EINVAL);
    ax_assert_eq!(
        InodeNumber::new(1).unwrap().to_group(0).unwrap_err().code,
        Errno::EINVAL
    );
}

#[axtest::def_test]
fn rsext4_blockgroup_table_and_stats_rules_hold() {
    use core::mem::size_of;

    use rsext4::{
        blockgroup_description::{
            BlockGroupDescTable, BlockGroupDescTableMut, BlockGroupStats, Ext4GroupDesc,
        },
        bmalloc::BGIndex,
        endian::DiskFormat,
        superblock::Ext4Superblock,
    };

    let desc_size = size_of::<Ext4GroupDesc>();
    let mut table_bytes = vec![0_u8; desc_size * 3];
    let descs = [
        Ext4GroupDesc {
            bg_free_blocks_count_lo: 2,
            bg_free_inodes_count_lo: 3,
            bg_used_dirs_count_lo: 1,
            bg_itable_unused_lo: 4,
            ..Default::default()
        },
        Ext4GroupDesc {
            bg_free_blocks_count_lo: 20,
            bg_free_inodes_count_lo: 0,
            bg_used_dirs_count_lo: 2,
            bg_flags: Ext4GroupDesc::EXT4_BG_INODE_UNINIT,
            ..Default::default()
        },
        Ext4GroupDesc {
            bg_free_blocks_count_lo: 8,
            bg_free_inodes_count_lo: 7,
            bg_used_dirs_count_lo: 3,
            bg_flags: Ext4GroupDesc::EXT4_BG_BLOCK_UNINIT,
            ..Default::default()
        },
    ];
    for (idx, desc) in descs.iter().enumerate() {
        let start = idx * desc_size;
        desc.to_disk_bytes(&mut table_bytes[start..start + desc_size]);
    }

    let table = BlockGroupDescTable::new(&table_bytes, desc_size, 3);
    ax_assert_eq!(table.group_count(), 3);
    ax_assert_eq!(table.desc_size(), desc_size);
    ax_assert_eq!(table.get_desc(BGIndex::new(3)).map(|_| ()), None);
    ax_assert_eq!(table.total_free_blocks(), 30);
    ax_assert_eq!(table.total_free_inodes(), 10);
    ax_assert_eq!(table.total_used_dirs(), 6);
    ax_assert_eq!(table.find_group_with_free_blocks(6), Some(BGIndex::new(1)));
    ax_assert_eq!(table.find_group_with_free_blocks(9), Some(BGIndex::new(1)));
    ax_assert_eq!(table.find_group_with_free_blocks(21), None);
    ax_assert_eq!(table.find_group_with_free_inodes(), Some(BGIndex::new(0)));
    ax_assert_eq!(table.iter().count(), 3);

    let stats =
        BlockGroupStats::from_desc(BGIndex::new(0), table.get_desc(BGIndex::new(0)).unwrap());
    ax_assert_eq!(stats.group_idx, BGIndex::new(0));
    ax_assert_eq!(stats.free_blocks, 2);
    ax_assert_eq!(stats.free_inodes, 3);
    ax_assert_eq!(stats.used_dirs, 1);
    ax_assert_eq!(stats.itable_unused, 4);
    ax_assert_eq!(stats.flags, 0);
    ax_assert_eq!(stats.used_blocks(16), 14);
    ax_assert_eq!(stats.used_inodes(16), 13);
    ax_assert!(stats.block_usage_percent(16) > 87.4);
    ax_assert!(stats.inode_usage_percent(16) > 81.2);
    ax_assert_eq!(stats.block_usage_percent(0), 0.0);
    ax_assert_eq!(stats.inode_usage_percent(0), 0.0);

    let mut mutable_table = BlockGroupDescTableMut::new(&mut table_bytes, desc_size, 3);
    ax_assert!(mutable_table.update_free_blocks(BGIndex::new(1), 0x1_0002));
    ax_assert!(mutable_table.update_free_inodes(BGIndex::new(1), 0x2_0003));
    ax_assert!(mutable_table.update_used_dirs(BGIndex::new(1), 0x3_0004));
    ax_assert!(mutable_table.increment_used_dirs(BGIndex::new(1)));
    ax_assert!(mutable_table.decrement_used_dirs(BGIndex::new(1)));
    ax_assert!(mutable_table.set_flags(
        BGIndex::new(1),
        Ext4GroupDesc::EXT4_BG_BLOCK_UNINIT | Ext4GroupDesc::EXT4_BG_INODE_ZEROED
    ));
    ax_assert!(mutable_table.clear_flags(BGIndex::new(1), Ext4GroupDesc::EXT4_BG_INODE_UNINIT));
    ax_assert!(!mutable_table.update_free_blocks(BGIndex::new(4), 1));
    ax_assert!(!mutable_table.update_free_inodes(BGIndex::new(4), 1));
    ax_assert!(!mutable_table.update_used_dirs(BGIndex::new(4), 1));
    ax_assert!(!mutable_table.increment_used_dirs(BGIndex::new(4)));
    ax_assert!(!mutable_table.decrement_used_dirs(BGIndex::new(4)));
    ax_assert!(!mutable_table.set_flags(BGIndex::new(4), 1));
    ax_assert!(!mutable_table.clear_flags(BGIndex::new(4), 1));

    let table = BlockGroupDescTable::new(&table_bytes, desc_size, 3);
    let desc = table.get_desc(BGIndex::new(1)).unwrap();
    ax_assert_eq!(desc.free_blocks_count(), 0x1_0002);
    ax_assert_eq!(desc.free_inodes_count(), 0x2_0003);
    ax_assert_eq!(desc.used_dirs_count(), 0x3_0004);
    ax_assert!(desc.is_block_bitmap_uninit());
    ax_assert!(!desc.is_inode_bitmap_uninit());
    ax_assert!(desc.is_inode_table_zeroed());

    let tiny_table = BlockGroupDescTable::new(&table_bytes[..desc_size - 1], desc_size, 1);
    ax_assert_eq!(tiny_table.get_desc(BGIndex::new(0)).map(|_| ()), None);

    let old_superblock = Ext4Superblock::default();
    let mut new_superblock = Ext4Superblock {
        s_feature_incompat: Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT,
        s_desc_size: 64,
        ..Default::default()
    };
    let mut checksum_desc = Ext4GroupDesc {
        bg_block_bitmap_csum_lo: 0x1234,
        bg_block_bitmap_csum_hi: 0x5678,
        bg_inode_bitmap_csum_lo: 0xabcd,
        bg_inode_bitmap_csum_hi: 0xef01,
        ..Default::default()
    };
    ax_assert_eq!(checksum_desc.block_bitmap_csum(&old_superblock), 0x1234);
    ax_assert_eq!(checksum_desc.inode_bitmap_csum(&old_superblock), 0xabcd);
    ax_assert_eq!(
        checksum_desc.block_bitmap_csum(&new_superblock),
        0x5678_1234
    );
    ax_assert_eq!(
        checksum_desc.inode_bitmap_csum(&new_superblock),
        0xef01_abcd
    );
    ax_assert!(checksum_desc.block_bitmap_csum_matches(&old_superblock, 0xffff_1234));
    ax_assert!(!checksum_desc.block_bitmap_csum_matches(&new_superblock, 0xffff_1234));

    new_superblock.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM;
    new_superblock.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_CSUM_SEED;
    new_superblock.s_checksum_seed = 0x3141_5926;
    checksum_desc.update_checksum(&new_superblock, 2, Some(&[0xaa; 16]), Some(&[0x55; 16]));
    ax_assert!(checksum_desc.verify_checksum(&new_superblock, 2).is_ok());
    checksum_desc.bg_checksum ^= 1;
    ax_assert!(checksum_desc.verify_checksum(&new_superblock, 2).is_err());
}

#[axtest::def_test]
fn rsext4_extent_tree_lookup_and_run_rules_hold() {
    use rsext4::{
        BLOCK_SIZE, BlockDevice, Ext4Result, Jbd2Dev,
        bmalloc::AbsoluteBN,
        disknode::{Ext4Extent, Ext4ExtentHeader, Ext4ExtentIdx, Ext4Inode, Ext4Timestamp},
        endian::DiskFormat,
        extents_tree::{ExtentNode, ExtentTree},
        loopfile::resolve_inode_block,
    };

    struct MemoryBlockDevice {
        blocks: Vec<u8>,
    }

    impl MemoryBlockDevice {
        fn new(block_count: usize) -> Self {
            Self {
                blocks: vec![0; block_count * BLOCK_SIZE],
            }
        }
    }

    impl BlockDevice for MemoryBlockDevice {
        fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            if buffer.len() < required {
                return Err(rsext4::Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + required;
            if end > self.blocks.len() {
                return Err(rsext4::Ext4Error::block_out_of_range(
                    block_id.raw().min(u64::from(u32::MAX)) as u32,
                    self.total_blocks(),
                ));
            }
            self.blocks[start..end].copy_from_slice(&buffer[..required]);
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            if buffer.len() < required {
                return Err(rsext4::Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + required;
            if end > self.blocks.len() {
                return Err(rsext4::Ext4Error::block_out_of_range(
                    block_id.raw().min(u64::from(u32::MAX)) as u32,
                    self.total_blocks(),
                ));
            }
            buffer[..required].copy_from_slice(&self.blocks[start..end]);
            Ok(())
        }

        fn open(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn close(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn total_blocks(&self) -> u64 {
            (self.blocks.len() / BLOCK_SIZE) as u64
        }

        fn block_size(&self) -> u32 {
            BLOCK_SIZE as u32
        }

        fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
            Ok(Ext4Timestamp::new(42, 0))
        }
    }

    fn store_leaf(inode: &mut Ext4Inode, extents: &[Ext4Extent]) {
        let header = Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: extents.len() as u16,
            eh_max: 4,
            eh_depth: 0,
            eh_generation: 0,
        };
        let node = rsext4::extents_tree::ExtentNode::Leaf {
            header,
            entries: extents.to_vec(),
        };
        ExtentTree::new(inode).store_root_to_inode(&node);
    }

    fn write_leaf_block(
        dev: &mut Jbd2Dev<MemoryBlockDevice>,
        block: AbsoluteBN,
        extents: &[Ext4Extent],
    ) {
        let mut bytes = vec![0_u8; BLOCK_SIZE];
        let header = Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: extents.len() as u16,
            eh_max: ((BLOCK_SIZE - Ext4ExtentHeader::disk_size()) / Ext4Extent::disk_size()) as u16,
            eh_depth: 0,
            eh_generation: 0,
        };
        header.to_disk_bytes(&mut bytes[0..Ext4ExtentHeader::disk_size()]);
        let mut offset = Ext4ExtentHeader::disk_size();
        for extent in extents {
            extent.to_disk_bytes(&mut bytes[offset..offset + Ext4Extent::disk_size()]);
            offset += Ext4Extent::disk_size();
        }
        dev.write_blocks(&bytes, block, 1, false).unwrap();
    }

    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemoryBlockDevice::new(8), false);
    let mut inode = Ext4Inode::default();
    ax_assert_eq!(
        ExtentTree::new(&mut inode)
            .initialized_runs_in_range(&mut dev, 5, 4)
            .unwrap(),
        Vec::new()
    );
    ax_assert!(
        ExtentTree::new(&mut inode)
            .find_extent(&mut dev, 1)
            .unwrap()
            .is_none()
    );

    inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    store_leaf(
        &mut inode,
        &[
            Ext4Extent::new(10, 100, 4),
            Ext4Extent::new(20, 200, 2),
            Ext4Extent {
                ee_block: 30,
                ee_len: Ext4Extent::encode_len(3, true).unwrap(),
                ee_start_hi: 0,
                ee_start_lo: 300,
            },
        ],
    );

    let mut tree = ExtentTree::new(&mut inode);
    ax_assert!(tree.find_extent(&mut dev, 9).unwrap().is_none());
    ax_assert_eq!(
        tree.find_extent(&mut dev, 10)
            .unwrap()
            .unwrap()
            .start_block(),
        100
    );
    ax_assert_eq!(
        tree.find_extent(&mut dev, 13)
            .unwrap()
            .unwrap()
            .start_block(),
        100
    );
    ax_assert!(tree.find_extent(&mut dev, 14).unwrap().is_none());
    let runs = tree.initialized_runs_in_range(&mut dev, 11, 30).unwrap();
    ax_assert_eq!(runs.len(), 2);
    ax_assert_eq!(runs[0].logical_start, 11);
    ax_assert_eq!(runs[0].physical_start, AbsoluteBN::new(101));
    ax_assert_eq!(runs[0].len, 3);
    ax_assert_eq!(runs[1].logical_start, 20);
    ax_assert_eq!(runs[1].physical_start, AbsoluteBN::new(200));
    ax_assert_eq!(runs[1].len, 2);
    ax_assert_eq!(
        resolve_inode_block(&mut dev, &mut inode, 11)
            .unwrap()
            .unwrap(),
        AbsoluteBN::new(101)
    );

    let mut zero_len_inode = Ext4Inode::empty_for_reuse(32);
    zero_len_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    store_leaf(
        &mut zero_len_inode,
        &[Ext4Extent {
            ee_block: 4,
            ee_len: 0,
            ee_start_hi: 0,
            ee_start_lo: 400,
        }],
    );
    ax_assert!(
        resolve_inode_block(&mut dev, &mut zero_len_inode, 4)
            .unwrap()
            .is_none()
    );

    let mut unwritten_inode = Ext4Inode::empty_for_reuse(32);
    unwritten_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    store_leaf(
        &mut unwritten_inode,
        &[Ext4Extent {
            ee_block: 7,
            ee_len: Ext4Extent::encode_len(2, true).unwrap(),
            ee_start_hi: 0,
            ee_start_lo: 700,
        }],
    );
    ax_assert!(
        resolve_inode_block(&mut dev, &mut unwritten_inode, 7)
            .unwrap()
            .is_none()
    );

    write_leaf_block(
        &mut dev,
        AbsoluteBN::new(2),
        &[Ext4Extent::new(0, 400, 2), Ext4Extent::new(5, 500, 3)],
    );
    write_leaf_block(
        &mut dev,
        AbsoluteBN::new(3),
        &[Ext4Extent::new(20, 700, 2), Ext4Extent::new(30, 900, 1)],
    );
    let mut indexed_inode = Ext4Inode::empty_for_reuse(32);
    indexed_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    let index_root = ExtentNode::Index {
        header: Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: 2,
            eh_max: 4,
            eh_depth: 1,
            eh_generation: 0,
        },
        entries: vec![
            Ext4ExtentIdx {
                ei_block: 0,
                ei_leaf_lo: 2,
                ei_leaf_hi: 0,
                ei_unused: 0,
            },
            Ext4ExtentIdx {
                ei_block: 20,
                ei_leaf_lo: 3,
                ei_leaf_hi: 0,
                ei_unused: 0,
            },
        ],
    };
    ExtentTree::new(&mut indexed_inode).store_root_to_inode(&index_root);

    let mut tree = ExtentTree::new(&mut indexed_inode);
    ax_assert_eq!(
        tree.find_extent(&mut dev, 6)
            .unwrap()
            .unwrap()
            .start_block(),
        500
    );
    ax_assert_eq!(
        tree.find_extent(&mut dev, 20)
            .unwrap()
            .unwrap()
            .start_block(),
        700
    );
    ax_assert!(tree.find_extent(&mut dev, 12).unwrap().is_none());
    let indexed_runs = tree.initialized_runs_in_range(&mut dev, 1, 30).unwrap();
    ax_assert_eq!(indexed_runs.len(), 4);
    ax_assert_eq!(indexed_runs[0].logical_start, 1);
    ax_assert_eq!(indexed_runs[0].physical_start, AbsoluteBN::new(401));
    ax_assert_eq!(indexed_runs[0].len, 1);
    ax_assert_eq!(indexed_runs[3].logical_start, 30);
    ax_assert_eq!(indexed_runs[3].physical_start, AbsoluteBN::new(900));
    ax_assert_eq!(indexed_runs[3].len, 1);
}

#[axtest::def_test]
fn rsext4_dirblock_checksum_edge_rules_hold() {
    use rsext4::{
        BLOCK_SIZE,
        checksum::{
            ext4_dirblock_csum32, ext4_metadata_block_csum32, ext4_metadata_csum32,
            ext4_update_dirblock_tail_checksum, update_ext4_dirblock_csum32,
            verify_ext4_dirblock_checksum, verify_ext4_dx_checksum,
        },
        crc32c::ext4_crc32c_seed_from_superblock,
        entries::Ext4DxRootInfo,
        superblock::Ext4Superblock,
    };

    let mut superblock = Ext4Superblock {
        s_uuid: [0x42; 16],
        ..Default::default()
    };
    let mut block = vec![0_u8; BLOCK_SIZE];
    block[0..4].copy_from_slice(&2_u32.to_le_bytes());
    block[4..6].copy_from_slice(&(BLOCK_SIZE as u16).to_le_bytes());
    block[6] = 1;
    block[7] = 2;
    ax_assert!(verify_ext4_dirblock_checksum(&superblock, 2, 3, &block));
    ax_assert_eq!(
        verify_ext4_dx_checksum(&superblock, 2, 3, &block),
        Some(true)
    );

    superblock.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM;
    superblock.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_CSUM_SEED;
    superblock.s_checksum_seed = 0x1234_5678;
    let csum = ext4_metadata_block_csum32(&superblock, b"metadata");
    ax_assert_eq!(
        csum,
        ext4_metadata_csum32(
            ext4_crc32c_seed_from_superblock(&superblock),
            &[b"metadata"]
        )
    );
    ax_assert_eq!(
        ext4_dirblock_csum32(&superblock, 2, 3, b"dir"),
        ext4_metadata_csum32(
            ext4_crc32c_seed_from_superblock(&superblock),
            &[&2_u32.to_le_bytes(), &3_u32.to_le_bytes(), b"dir"]
        )
    );
    ax_assert!(!verify_ext4_dirblock_checksum(
        &superblock,
        2,
        3,
        &[0_u8; 8]
    ));

    block.fill(0);
    block[BLOCK_SIZE - 5] = 0xDE;
    update_ext4_dirblock_csum32(&superblock, 2, 3, &mut block);
    ax_assert!(verify_ext4_dirblock_checksum(&superblock, 2, 3, &block));
    block[0] ^= 1;
    ax_assert!(!verify_ext4_dirblock_checksum(&superblock, 2, 3, &block));

    let tail_offset = 64;
    block.fill(0);
    ext4_update_dirblock_tail_checksum(&superblock, 4, 5, &mut block, tail_offset);
    let stored = u32::from_le_bytes([
        block[tail_offset + 8],
        block[tail_offset + 9],
        block[tail_offset + 10],
        block[tail_offset + 11],
    ]);
    ax_assert_eq!(
        stored,
        ext4_dirblock_csum32(&superblock, 4, 5, &block[..tail_offset])
    );
    let before = block.clone();
    ext4_update_dirblock_tail_checksum(&superblock, 4, 5, &mut block, BLOCK_SIZE - 4);
    ax_assert_eq!(block, before);

    block.fill(0);
    block[4..6].copy_from_slice(&(BLOCK_SIZE as u16).to_le_bytes());
    block[8..10].copy_from_slice(&2_u16.to_le_bytes());
    block[10..12].copy_from_slice(&1_u16.to_le_bytes());
    block[12..16].copy_from_slice(&0x1111_1111_u32.to_le_bytes());
    block[16..20].copy_from_slice(&7_u32.to_le_bytes());
    let dx_tail_offset = 8 + 2 * core::mem::size_of::<rsext4::entries::Ext4DxEntry>();
    let zero_checksum = [0_u8; 4];
    let dx_checksum = ext4_metadata_csum32(
        ext4_crc32c_seed_from_superblock(&superblock),
        &[
            &2_u32.to_le_bytes(),
            &3_u32.to_le_bytes(),
            &block[..16],
            &block[dx_tail_offset..dx_tail_offset + 4],
            &zero_checksum,
        ],
    );
    block[dx_tail_offset + 4..dx_tail_offset + 8].copy_from_slice(&dx_checksum.to_le_bytes());
    ax_assert_eq!(
        verify_ext4_dx_checksum(&superblock, 2, 3, &block),
        Some(true)
    );
    block[dx_tail_offset + 4] ^= 1;
    ax_assert_eq!(
        verify_ext4_dx_checksum(&superblock, 2, 3, &block),
        Some(false)
    );
    block[10..12].copy_from_slice(&3_u16.to_le_bytes());
    ax_assert_eq!(
        verify_ext4_dx_checksum(&superblock, 2, 3, &block),
        Some(false)
    );

    block.fill(0);
    block[4..6].copy_from_slice(&12_u16.to_le_bytes());
    block[24..28].copy_from_slice(&0_u32.to_le_bytes());
    block[29] = Ext4DxRootInfo::INFO_LENGTH;
    block[32..34].copy_from_slice(&1_u16.to_le_bytes());
    block[34..36].copy_from_slice(&0_u16.to_le_bytes());
    ax_assert_eq!(
        verify_ext4_dx_checksum(&superblock, 2, 3, &block),
        Some(false)
    );
    block[29] = 0;
    ax_assert_eq!(verify_ext4_dx_checksum(&superblock, 2, 3, &block), None);
}

#[axtest::def_test]
fn rsext4_mounted_filesystem_file_dir_and_metadata_rules_hold() {
    use core::cell::Cell;

    use rsext4::{
        BLOCK_SIZE, BlockDevice, Errno, Ext4Result, Jbd2Dev,
        bmalloc::AbsoluteBN,
        create_symbol_link, create_symbol_link_with_owner, delete_dir, delete_file,
        dir::get_inode_with_num,
        disknode::{
            Ext4Extent, Ext4ExtentHeader, Ext4ExtentIdx, Ext4Inode, Ext4TimeSpec, Ext4Timestamp,
        },
        endian::DiskFormat,
        entries::Ext4DirEntry2,
        extents_tree::{ExtentNode, ExtentTree},
        file::build_file_block_mapping_with_inode_num,
        find_file, link,
        loopfile::{resolve_inode_block, resolve_inode_block_allextend},
        metadata::{chmod, chown},
        mkdir, mkdir_with_owner, mkfile, mkfile_with_owner, mkfs, read_file, read_inode_data_into,
        rename, set_flags, set_project,
        superblock::Ext4Superblock,
        truncate, utimens, write_file,
    };

    struct MemoryBlockDevice {
        blocks: Vec<u8>,
        now: Cell<i64>,
    }

    impl MemoryBlockDevice {
        fn new(block_count: usize) -> Self {
            Self {
                blocks: vec![0; block_count * BLOCK_SIZE],
                now: Cell::new(1_800_000_000),
            }
        }
    }

    impl BlockDevice for MemoryBlockDevice {
        fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            if buffer.len() < required {
                return Err(rsext4::Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + required;
            self.blocks[start..end].copy_from_slice(&buffer[..required]);
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            if buffer.len() < required {
                return Err(rsext4::Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + required;
            if end > self.blocks.len() {
                return Err(rsext4::Ext4Error::block_out_of_range(
                    block_id.raw().min(u64::from(u32::MAX)) as u32,
                    self.total_blocks(),
                ));
            }
            buffer[..required].copy_from_slice(&self.blocks[start..end]);
            Ok(())
        }

        fn open(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn close(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn total_blocks(&self) -> u64 {
            (self.blocks.len() / BLOCK_SIZE) as u64
        }

        fn block_size(&self) -> u32 {
            BLOCK_SIZE as u32
        }

        fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
            let sec = self.now.get();
            self.now.set(sec + 1);
            Ok(Ext4Timestamp::new(sec, 0))
        }
    }

    let mut device = Jbd2Dev::initial_jbd2dev(0, MemoryBlockDevice::new(16 * 1024), false);
    mkfs(&mut device).unwrap();
    let mut fs = rsext4::api::fs_mount(&mut device).unwrap();

    ax_assert!(!rsext4::Ext4FileSystem::device_has_error_state(&mut device).unwrap());
    ax_assert!(fs.file_entries_exist(&mut device, "/").unwrap());
    fs.make_base_dir();
    let stats = fs.statfs();
    ax_assert_eq!(stats.block_size, BLOCK_SIZE as u64);
    ax_assert_eq!(stats.block_groups, fs.group_count);
    ax_assert!(stats.total_blocks >= stats.free_blocks);
    ax_assert!(stats.total_inodes >= stats.free_inodes);
    ax_assert!(
        fs.get_group_desc(rsext4::bmalloc::BGIndex::new(0))
            .is_some()
    );
    ax_assert!(
        fs.get_group_desc(rsext4::bmalloc::BGIndex::new(u32::MAX))
            .is_none()
    );
    ax_assert!(
        fs.get_group_desc_mut(rsext4::bmalloc::BGIndex::new(u32::MAX))
            .is_none()
    );
    ax_assert!(
        fs.inode_num_already_allocated(&mut device, rsext4::bmalloc::InodeNumber::new(2).unwrap())
    );
    let saved_inode_bitmap = fs.group_descs[0].bg_inode_bitmap_lo;
    fs.group_descs[0].bg_inode_bitmap_lo = u32::MAX;
    fs.bitmap_cache.clear();
    let _ =
        fs.inode_num_already_allocated(&mut device, rsext4::bmalloc::InodeNumber::new(2).unwrap());
    fs.group_descs[0].bg_inode_bitmap_lo = saved_inode_bitmap;
    fs.bitmap_cache.clear();
    let saved_inodes_per_group = fs.superblock.s_inodes_per_group;
    fs.superblock.s_inodes_per_group = 1;
    let _ =
        fs.inode_num_already_allocated(&mut device, rsext4::bmalloc::InodeNumber::new(2).unwrap());
    fs.superblock.s_inodes_per_group = saved_inodes_per_group;
    ax_assert!(!fs.inode_num_already_allocated(
        &mut device,
        rsext4::bmalloc::InodeNumber::new(u32::MAX).unwrap()
    ));
    ax_assert!(
        fs.get_inode_by_num(
            &mut device,
            rsext4::bmalloc::InodeNumber::new(u32::MAX).unwrap()
        )
        .is_err()
    );
    ax_assert!(
        fs.modify_inode(
            &mut device,
            rsext4::bmalloc::InodeNumber::new(u32::MAX).unwrap(),
            |_| {}
        )
        .is_err()
    );
    ax_assert!(find_file(&mut fs, &mut device, "/").unwrap().is_dir());
    ax_assert_eq!(
        find_file(&mut fs, &mut device, "/missing")
            .unwrap_err()
            .code,
        Errno::ENOENT
    );

    mkdir(&mut device, &mut fs, "/cov").unwrap();
    mkdir_with_owner(&mut device, &mut fs, "/cov/sub", 1000, 1001).unwrap();
    ax_assert!(
        get_inode_with_num(&mut fs, &mut device, "/cov/sub")
            .unwrap()
            .unwrap()
            .1
            .is_dir()
    );
    ax_assert!(
        get_inode_with_num(&mut fs, &mut device, "/cov/sub/./../sub")
            .unwrap()
            .unwrap()
            .1
            .is_dir()
    );

    let file_inode = mkfile(&mut device, &mut fs, "/cov/sub/file", Some(b"hello"), None).unwrap();
    ax_assert!(file_inode.is_file());
    ax_assert_eq!(
        read_file(&mut device, &mut fs, "/cov/sub/file").unwrap(),
        b"hello"
    );

    write_file(&mut device, &mut fs, "/cov/sub/file", 5, b" world").unwrap();
    ax_assert_eq!(
        read_file(&mut device, &mut fs, "/cov/sub/file").unwrap(),
        b"hello world"
    );

    let (file_ino, _) = get_inode_with_num(&mut fs, &mut device, "/cov/sub/file")
        .unwrap()
        .unwrap();
    let mut partial = [0_u8; 5];
    let copied = read_inode_data_into(&mut device, &mut fs, file_ino, 6, &mut partial).unwrap();
    ax_assert_eq!(copied, 5);
    ax_assert_eq!(&partial, b"world");
    let copied = read_inode_data_into(&mut device, &mut fs, file_ino, 99, &mut partial).unwrap();
    ax_assert_eq!(copied, 0);
    ax_assert!(
        get_inode_with_num(&mut fs, &mut device, "/cov/sub/file/child")
            .unwrap()
            .is_none()
    );
    let saved_group_descs = core::mem::take(&mut fs.group_descs);
    ax_assert!(fs.get_inode_by_num(&mut device, file_ino).is_err());
    ax_assert!(fs.modify_inode(&mut device, file_ino, |_| {}).is_err());
    fs.group_descs = saved_group_descs;

    chmod(&mut device, &mut fs, "/cov/sub/file", 0o600).unwrap();
    chown(
        &mut device,
        &mut fs,
        "/cov/sub/file",
        Some(2000),
        Some(2001),
    )
    .unwrap();
    ax_assert_eq!(
        set_project(&mut device, &mut fs, "/cov/sub/file", 77)
            .unwrap_err()
            .code,
        Errno::EOPNOTSUPP
    );
    fs.superblock.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT;
    set_project(&mut device, &mut fs, "/cov/sub/file", 77).unwrap();
    let (_, projected_inode) = get_inode_with_num(&mut fs, &mut device, "/cov/sub/file")
        .unwrap()
        .unwrap();
    ax_assert_eq!(projected_inode.i_projid, 77);
    fs.superblock.s_feature_ro_compat &= !Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT;
    set_flags(&mut device, &mut fs, "/cov/sub/file", 0).unwrap();
    ax_assert_eq!(
        set_flags(
            &mut device,
            &mut fs,
            "/cov/sub/file",
            !Ext4Inode::EXT4_FL_USER_VISIBLE
        )
        .unwrap_err()
        .code,
        Errno::EINVAL
    );
    mkfile(&mut device, &mut fs, "/cov/sub/noatime", Some(b"n"), None).unwrap();
    set_flags(
        &mut device,
        &mut fs,
        "/cov/sub/noatime",
        Ext4Inode::EXT4_NOATIME_FL | Ext4Inode::EXT4_DIRSYNC_FL,
    )
    .unwrap();
    let (_, noatime_inode) = get_inode_with_num(&mut fs, &mut device, "/cov/sub/noatime")
        .unwrap()
        .unwrap();
    ax_assert_ne!(noatime_inode.i_flags & Ext4Inode::EXT4_NOATIME_FL, 0);
    ax_assert_eq!(noatime_inode.i_flags & Ext4Inode::EXT4_DIRSYNC_FL, 0);
    utimens(
        &mut device,
        &mut fs,
        "/cov/sub/file",
        Ext4TimeSpec::Set(Ext4Timestamp::new(1_800_000_100, 123)),
        Ext4TimeSpec::Now,
    )
    .unwrap();
    utimens(
        &mut device,
        &mut fs,
        "/cov/sub/file",
        Ext4TimeSpec::Omit,
        Ext4TimeSpec::Omit,
    )
    .unwrap();

    mkfile_with_owner(
        &mut device,
        &mut fs,
        "/cov/sub/char",
        None,
        Some(Ext4DirEntry2::EXT4_FT_CHRDEV),
        3000,
        3001,
    )
    .unwrap();
    let (_, char_inode) = get_inode_with_num(&mut fs, &mut device, "/cov/sub/char")
        .unwrap()
        .unwrap();
    ax_assert_eq!(char_inode.i_mode & Ext4Inode::S_IFMT, Ext4Inode::S_IFCHR);
    ax_assert_eq!(char_inode.uid(), 3000);
    ax_assert_eq!(char_inode.gid(), 3001);
    mkfile(
        &mut device,
        &mut fs,
        "/cov/sub/socket",
        None,
        Some(Ext4DirEntry2::EXT4_FT_SOCK),
    )
    .unwrap();
    mkfile(
        &mut device,
        &mut fs,
        "/cov/sub/block",
        None,
        Some(Ext4DirEntry2::EXT4_FT_BLKDEV),
    )
    .unwrap();
    mkfile(
        &mut device,
        &mut fs,
        "/cov/sub/fifo",
        None,
        Some(Ext4DirEntry2::EXT4_FT_FIFO),
    )
    .unwrap();
    mkfile(
        &mut device,
        &mut fs,
        "/cov/sub/unknown-kind",
        None,
        Some(Ext4DirEntry2::EXT4_FT_UNKNOWN),
    )
    .unwrap();
    ax_assert_eq!(
        mkfile(&mut device, &mut fs, "/", None, None)
            .unwrap_err()
            .code,
        Errno::EINVAL
    );
    ax_assert_eq!(
        mkfile(&mut device, &mut fs, "/cov/sub/file", None, None)
            .unwrap_err()
            .code,
        Errno::EEXIST
    );
    ax_assert_eq!(
        mkfile(&mut device, &mut fs, "relative-file", None, None)
            .unwrap_err()
            .code,
        Errno::EINVAL
    );
    fs.superblock.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT;
    let (sub_ino, _) = get_inode_with_num(&mut fs, &mut device, "/cov/sub")
        .unwrap()
        .unwrap();
    fs.modify_inode(&mut device, sub_ino, |inode| {
        inode.i_flags |= Ext4Inode::EXT4_PROJINHERIT_FL;
        inode.i_projid = 1234;
    })
    .unwrap();
    mkfile(
        &mut device,
        &mut fs,
        "/cov/sub/project-child",
        Some(b"p"),
        None,
    )
    .unwrap();
    let (_, project_child) = get_inode_with_num(&mut fs, &mut device, "/cov/sub/project-child")
        .unwrap()
        .unwrap();
    ax_assert_eq!(project_child.i_projid, 1234);

    link(&mut fs, &mut device, "/cov/sub/hard", "/cov/sub/file").unwrap();
    ax_assert_eq!(
        read_file(&mut device, &mut fs, "/cov/sub/hard").unwrap(),
        b"hello world"
    );
    ax_assert_eq!(
        link(&mut fs, &mut device, "/cov/sub/hard", "/cov/sub/file")
            .unwrap_err()
            .code,
        Errno::EEXIST
    );
    ax_assert_eq!(
        link(&mut fs, &mut device, "/cov/sub/dir-hard", "/cov/sub")
            .unwrap_err()
            .code,
        Errno::EACCES
    );
    ax_assert_eq!(
        link(
            &mut fs,
            &mut device,
            "/cov/missing-parent/hard",
            "/cov/sub/file"
        )
        .unwrap_err()
        .code,
        Errno::ENOENT
    );
    ax_assert_eq!(
        link(
            &mut fs,
            &mut device,
            "/cov/sub/no-target",
            "/cov/sub/no-file"
        )
        .unwrap_err()
        .code,
        Errno::ENOENT
    );

    create_symbol_link(&mut device, &mut fs, "/cov/sub/file", "/cov/sub/sym").unwrap();
    ax_assert_eq!(
        read_file(&mut device, &mut fs, "/cov/sub/sym").unwrap(),
        b"hello world"
    );
    link(&mut fs, &mut device, "/sym-root-hard", "/cov/sub/sym").unwrap();
    link(&mut fs, &mut device, "/cov/sub/sym-hard", "/cov/sub/sym").unwrap();
    ax_assert_eq!(
        read_file(&mut device, &mut fs, "/cov/sub/sym-hard").unwrap(),
        b"hello world"
    );
    let long_target = "/cov/sub/long-".to_string() + &"segment".repeat(12);
    mkfile(&mut device, &mut fs, &long_target, Some(b"long"), None).unwrap();
    create_symbol_link_with_owner(
        &mut device,
        &mut fs,
        &long_target,
        "/cov/sub/long-sym",
        7,
        8,
    )
    .unwrap();
    create_symbol_link(&mut device, &mut fs, "/cov/sub/file", "/root-sym").unwrap();
    create_symbol_link(
        &mut device,
        &mut fs,
        "/cov/sub/project-child",
        "/cov/sub/project-sym",
    )
    .unwrap();
    let (_, project_sym) = get_inode_with_num(&mut fs, &mut device, "/cov/sub/project-sym")
        .unwrap()
        .unwrap();
    ax_assert_eq!(project_sym.i_projid, 1234);
    fs.superblock.s_feature_ro_compat &= !Ext4Superblock::EXT4_FEATURE_RO_COMPAT_PROJECT;
    ax_assert_eq!(
        read_file(&mut device, &mut fs, "/cov/sub/long-sym").unwrap(),
        b"long"
    );
    ax_assert_eq!(
        create_symbol_link(&mut device, &mut fs, "/cov/sub/no-src", "/cov/sub/bad-sym")
            .unwrap_err()
            .code,
        Errno::EINVAL
    );

    rename(&mut device, &mut fs, "/cov/sub/file", "/cov/sub/renamed").unwrap();
    ax_assert!(
        get_inode_with_num(&mut fs, &mut device, "/cov/sub/file")
            .unwrap()
            .is_none()
    );
    ax_assert!(
        get_inode_with_num(&mut fs, &mut device, "/cov/sub/renamed")
            .unwrap()
            .is_some()
    );

    let mut mapped_inode = Ext4Inode::empty_for_reuse(32);
    build_file_block_mapping_with_inode_num(&mut fs, &mut mapped_inode, file_ino, &[], &mut device);
    ax_assert_eq!(mapped_inode.blocks_count(), 0);
    let map_block_a = fs.alloc_block(&mut device).unwrap();
    let _map_gap = fs.alloc_block(&mut device).unwrap();
    let map_block_b = fs.alloc_block(&mut device).unwrap();
    build_file_block_mapping_with_inode_num(
        &mut fs,
        &mut mapped_inode,
        file_ino,
        &[map_block_a, map_block_b],
        &mut device,
    );
    ax_assert!(mapped_inode.have_extend_header_and_use_extend());
    let mapped_blocks =
        resolve_inode_block_allextend(&mut fs, &mut device, &mut mapped_inode).unwrap();
    ax_assert_eq!(mapped_blocks.len(), 2);

    let mut non_extent_inode = Ext4Inode::default();
    ax_assert_eq!(
        resolve_inode_block(&mut device, &mut non_extent_inode, 0)
            .unwrap_err()
            .code,
        Errno::EOPNOTSUPP
    );
    ax_assert!(
        resolve_inode_block_allextend(&mut fs, &mut device, &mut non_extent_inode)
            .unwrap()
            .is_empty()
    );
    let mut empty_extent_inode = Ext4Inode::empty_for_reuse(32);
    empty_extent_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    empty_extent_inode.write_extend_header();
    ax_assert!(
        resolve_inode_block(&mut device, &mut empty_extent_inode, 0)
            .unwrap()
            .is_none()
    );
    let mut skipped_extent_inode = Ext4Inode::empty_for_reuse(32);
    skipped_extent_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    let skipped_node = ExtentNode::Leaf {
        header: Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: 2,
            eh_max: 4,
            eh_depth: 0,
            eh_generation: 0,
        },
        entries: vec![
            Ext4Extent {
                ee_block: 0,
                ee_len: 0,
                ee_start_hi: 0,
                ee_start_lo: 0,
            },
            Ext4Extent {
                ee_block: 4,
                ee_len: Ext4Extent::encode_len(1, true).unwrap(),
                ee_start_hi: 0,
                ee_start_lo: 4,
            },
        ],
    };
    ExtentTree::new(&mut skipped_extent_inode).store_root_to_inode(&skipped_node);
    ax_assert!(
        resolve_inode_block_allextend(&mut fs, &mut device, &mut skipped_extent_inode)
            .unwrap()
            .is_empty()
    );
    let leaf_a = fs.alloc_block(&mut device).unwrap();
    let leaf_b = fs.alloc_block(&mut device).unwrap();
    for (leaf, extent) in [
        (leaf_a, Ext4Extent::new(0, 800, 1)),
        (leaf_b, Ext4Extent::new(8, 900, 2)),
    ] {
        let mut leaf_bytes = vec![0_u8; BLOCK_SIZE];
        Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: 1,
            eh_max: ((BLOCK_SIZE - Ext4ExtentHeader::disk_size()) / Ext4Extent::disk_size()) as u16,
            eh_depth: 0,
            eh_generation: 0,
        }
        .to_disk_bytes(&mut leaf_bytes[0..Ext4ExtentHeader::disk_size()]);
        extent.to_disk_bytes(
            &mut leaf_bytes[Ext4ExtentHeader::disk_size()
                ..Ext4ExtentHeader::disk_size() + Ext4Extent::disk_size()],
        );
        device.write_blocks(&leaf_bytes, leaf, 1, false).unwrap();
    }
    let mut indexed_resolve_inode = Ext4Inode::empty_for_reuse(32);
    indexed_resolve_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    let indexed_root = ExtentNode::Index {
        header: Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: 2,
            eh_max: 4,
            eh_depth: 1,
            eh_generation: 0,
        },
        entries: vec![
            Ext4ExtentIdx {
                ei_block: 0,
                ei_leaf_lo: leaf_a.to_u32().unwrap(),
                ei_leaf_hi: 0,
                ei_unused: 0,
            },
            Ext4ExtentIdx {
                ei_block: 8,
                ei_leaf_lo: leaf_b.to_u32().unwrap(),
                ei_leaf_hi: 0,
                ei_unused: 0,
            },
        ],
    };
    ExtentTree::new(&mut indexed_resolve_inode).store_root_to_inode(&indexed_root);
    let indexed_blocks =
        resolve_inode_block_allextend(&mut fs, &mut device, &mut indexed_resolve_inode).unwrap();
    ax_assert_eq!(indexed_blocks.get(&0).copied(), Some(AbsoluteBN::new(800)));
    ax_assert_eq!(indexed_blocks.get(&9).copied(), Some(AbsoluteBN::new(901)));

    mkdir(&mut device, &mut fs, "/cov/other").unwrap();
    mkdir(&mut device, &mut fs, "/cov/other/child").unwrap();
    rename(
        &mut device,
        &mut fs,
        "/cov/other/child",
        "/cov/sub/moved-dir",
    )
    .unwrap();
    ax_assert!(
        get_inode_with_num(&mut fs, &mut device, "/cov/sub/moved-dir")
            .unwrap()
            .unwrap()
            .1
            .is_dir()
    );
    mkfile(
        &mut device,
        &mut fs,
        "/cov/sub/replace-src",
        Some(b"src"),
        None,
    )
    .unwrap();
    mkfile(
        &mut device,
        &mut fs,
        "/cov/sub/replace-dst",
        Some(b"dst"),
        None,
    )
    .unwrap();
    rename(
        &mut device,
        &mut fs,
        "/cov/sub/replace-src",
        "/cov/sub/replace-dst",
    )
    .unwrap();
    ax_assert_eq!(
        read_file(&mut device, &mut fs, "/cov/sub/replace-dst").unwrap(),
        b"src"
    );
    mkdir(&mut device, &mut fs, "/cov/sub/non-empty-dir").unwrap();
    mkfile(
        &mut device,
        &mut fs,
        "/cov/sub/non-empty-dir/live",
        Some(b"x"),
        None,
    )
    .unwrap();
    ax_assert_eq!(
        rename(
            &mut device,
            &mut fs,
            "/cov/sub/replace-dst",
            "/cov/sub/non-empty-dir",
        )
        .unwrap_err()
        .code,
        Errno::ENOTDIR
    );
    ax_assert_eq!(
        rename(
            &mut device,
            &mut fs,
            "/cov/sub/non-empty-dir",
            "/cov/sub/replace-dst",
        )
        .unwrap_err()
        .code,
        Errno::EISDIR
    );
    mkdir(&mut device, &mut fs, "/cov/sub/src-dir").unwrap();
    ax_assert_eq!(
        rename(
            &mut device,
            &mut fs,
            "/cov/sub/src-dir",
            "/cov/sub/non-empty-dir",
        )
        .unwrap_err()
        .code,
        Errno::ENOTEMPTY
    );
    ax_assert_eq!(
        rename(&mut device, &mut fs, "/cov/sub/src-dir", "bad-new")
            .unwrap_err()
            .code,
        Errno::EINVAL
    );
    ax_assert_eq!(
        rename(&mut device, &mut fs, "/", "/cov/sub/root-move")
            .unwrap_err()
            .code,
        Errno::EINVAL
    );
    mkdir(&mut device, &mut fs, "/cov/sub/replace-empty-src").unwrap();
    mkdir(&mut device, &mut fs, "/cov/sub/replace-empty-dst").unwrap();
    rename(
        &mut device,
        &mut fs,
        "/cov/sub/replace-empty-src",
        "/cov/sub/replace-empty-dst",
    )
    .unwrap();
    ax_assert!(
        get_inode_with_num(&mut fs, &mut device, "/cov/sub/replace-empty-dst")
            .unwrap()
            .unwrap()
            .1
            .is_dir()
    );
    ax_assert!(rename(&mut device, &mut fs, "bad-old", "/cov/sub/bad-new").is_err());

    truncate(&mut device, &mut fs, "/cov/sub/renamed", 4).unwrap();
    let truncated = read_file(&mut device, &mut fs, "/cov/sub/renamed").unwrap();
    ax_assert_eq!(truncated.as_slice(), b"hell");
    truncate(&mut device, &mut fs, "/cov/sub/renamed", 4).unwrap();
    truncate(&mut device, &mut fs, "/cov/sub/renamed", 0).unwrap();
    ax_assert_eq!(
        read_file(&mut device, &mut fs, "/cov/sub/renamed").unwrap(),
        Vec::new()
    );
    ax_assert_eq!(
        truncate(&mut device, &mut fs, "/cov/sub/missing", 1)
            .unwrap_err()
            .code,
        Errno::ENOENT
    );
    ax_assert_eq!(
        write_file(&mut device, &mut fs, "/cov/sub/missing", 0, b"x")
            .unwrap_err()
            .code,
        Errno::ENOENT
    );

    let mut api_file = rsext4::api::open(&mut device, &mut fs, "/cov/sub/api", true).unwrap();
    ax_assert_eq!(api_file.offset, 0);
    rsext4::api::write_at(&mut device, &mut fs, &mut api_file, b"abcdef").unwrap();
    ax_assert_eq!(api_file.offset, 6);
    rsext4::api::lseek(&mut api_file, 2).unwrap();
    ax_assert_eq!(
        rsext4::api::read_at(&mut device, &mut fs, &mut api_file, 3).unwrap(),
        b"cde"
    );
    ax_assert_eq!(api_file.offset, 5);
    ax_assert_eq!(
        rsext4::api::read(&mut device, &mut fs, "/cov/sub/api").unwrap(),
        b"abcdef"
    );
    rsext4::api::lseek(&mut api_file, 99).unwrap();
    ax_assert_eq!(
        rsext4::api::read_at(&mut device, &mut fs, &mut api_file, 4).unwrap(),
        Vec::new()
    );
    ax_assert_eq!(
        rsext4::api::read_at(&mut device, &mut fs, &mut api_file, 0).unwrap(),
        Vec::new()
    );
    let open_error = match rsext4::api::open(&mut device, &mut fs, "/cov/sub/no-api", false) {
        Ok(_) => panic!("opening a missing file without create should fail"),
        Err(error) => error,
    };
    ax_assert_eq!(open_error.code, Errno::ENOENT);

    delete_file(&mut fs, &mut device, "/cov/sub/api").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/noatime").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/char").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/socket").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/block").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/fifo").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/unknown-kind").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/project-child").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/project-sym").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/hard").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/long-sym").unwrap();
    delete_file(&mut fs, &mut device, &long_target).unwrap();
    delete_file(&mut fs, &mut device, "/root-sym").unwrap();
    delete_file(&mut fs, &mut device, "/sym-root-hard").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/sym-hard").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/sym").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/renamed").unwrap();
    delete_file(&mut fs, &mut device, "/cov/sub/non-empty-dir/live").unwrap();
    delete_dir(&mut fs, &mut device, "/cov/sub/src-dir").unwrap();
    delete_dir(&mut fs, &mut device, "/cov/sub/non-empty-dir").unwrap();
    delete_dir(&mut fs, &mut device, "/cov/sub/replace-empty-dst").unwrap();
    delete_dir(&mut fs, &mut device, "/cov/sub/moved-dir").unwrap();
    delete_dir(&mut fs, &mut device, "/cov/sub").unwrap();
    delete_dir(&mut fs, &mut device, "/cov/other").unwrap();
    delete_dir(&mut fs, &mut device, "/cov").unwrap();

    rsext4::api::fs_umount(fs, &mut device).unwrap();
}
