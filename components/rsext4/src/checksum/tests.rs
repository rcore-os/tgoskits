use super::*;
use crate::{
    BLOCK_SIZE,
    bmalloc::InodeNumber,
    crc32c::ext4_crc32c_seed_from_superblock,
    disknode::Ext4Inode,
    endian::DiskFormat,
    entries::{Ext4DirEntryTail, Ext4DxEntry},
    error::Errno,
    jbd2::jbdstruct::*,
    superblock::Ext4Superblock,
};

fn metadata_csum_superblock() -> Ext4Superblock {
    Ext4Superblock {
        s_magic: Ext4Superblock::EXT4_SUPER_MAGIC,
        s_inode_size: Ext4Inode::LARGE_INODE_SIZE,
        s_clusters_per_group: 8192,
        s_inodes_per_group: 2048,
        s_blocks_count_lo: 1024,
        s_free_blocks_count_lo: 900,
        s_free_inodes_count: 2000,
        s_feature_ro_compat: Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE
            | Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM,
        s_uuid: [
            0x5A, 0xC3, 0x11, 0x7E, 0x90, 0xAB, 0x4D, 0x2F, 0x10, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88,
        ],
        ..Default::default()
    }
}

fn sample_inode() -> Ext4Inode {
    let mut inode = Ext4Inode {
        i_mode: Ext4Inode::S_IFREG | 0o764,
        i_uid: 0x1234,
        i_size_lo: 0x5566_7788,
        i_atime: 100,
        i_ctime: 200,
        i_mtime: 300,
        i_dtime: 0,
        i_gid: 0x5678,
        i_links_count: 2,
        i_blocks_lo: 16,
        i_flags: Ext4Inode::EXT4_EXTENTS_FL,
        l_i_version: 7,
        i_generation: 0xCAFE_BABE,
        i_size_high: 1,
        l_i_blocks_high: 2,
        l_i_uid_high: 0x9ABC,
        l_i_gid_high: 0xDEF0,
        i_extra_isize: Ext4Inode::required_extra_isize(Ext4Inode::FIELD_END_I_PROJID),
        i_crtime: 400,
        i_projid: 123,
        ..Default::default()
    };
    inode.write_extend_header();
    inode
}

#[test]
fn superblock_checksum_round_trips_and_corruption_returns_euclean() {
    // Test idea: once a checksummed superblock is serialized, verification should accept
    // the stored value and reject a single-byte metadata mutation with EUCLEAN.
    let mut sb = metadata_csum_superblock();
    sb.update_checksum();

    let verified = sb.verify_superblock().unwrap();
    assert_eq!(verified.s_checksum, sb.s_checksum);

    let mut bytes = [0u8; Ext4Superblock::SUPERBLOCK_SIZE];
    sb.to_disk_bytes(&mut bytes);
    let stored = u32::from_le_bytes(
        bytes[Ext4Superblock::SUPERBLOCK_SIZE - 4..]
            .try_into()
            .unwrap(),
    );
    assert_eq!(stored, sb.s_checksum);

    let mut corrupted = sb;
    corrupted.s_free_blocks_count_lo ^= 1;
    let err = corrupted.verify_superblock().unwrap_err();
    assert_eq!(err.code, Errno::EUCLEAN);
}

#[test]
fn inode_checksum_is_split_and_persisted_to_disk() {
    // Test idea: inode checksum helpers must both compute the expected CRC32C value and
    // split it into the low/high on-disk checksum fields.
    let sb = metadata_csum_superblock();
    let inode_num = InodeNumber::new(42).unwrap();
    let generation = 0x1020_3040;
    let inode_size = Ext4Inode::LARGE_INODE_SIZE as usize;
    let mut inode = sample_inode();
    inode.i_generation = generation;

    ext4_update_inode_checksum(&sb, inode_num, generation, &mut inode, inode_size);

    let expected = ext4_inode_csum32(&sb, inode_num, generation, &inode, inode_size);
    assert_eq!(inode.l_i_checksum_lo, (expected & 0xFFFF) as u16);
    assert_eq!(inode.i_checksum_hi, ((expected >> 16) & 0xFFFF) as u16);

    let mut bytes = [0u8; Ext4Inode::LARGE_INODE_SIZE as usize];
    inode.to_disk_bytes(&mut bytes);
    assert_eq!(
        u16::from_le_bytes(bytes[124..126].try_into().unwrap()),
        inode.l_i_checksum_lo
    );
    assert_eq!(
        u16::from_le_bytes(bytes[130..132].try_into().unwrap()),
        inode.i_checksum_hi
    );
}

#[test]
fn dirblock_checksum_is_stored_and_detects_corruption() {
    // Test idea: directory block CRC must be written into the tail and verification must
    // fail after mutating any protected byte in the directory payload.
    let sb = metadata_csum_superblock();
    let ino = 11;
    let generation = 0x5566_7788;
    let mut block = [0u8; BLOCK_SIZE];
    let tail_offset = BLOCK_SIZE - Ext4DirEntryTail::TAIL_LEN as usize;

    block[..12].copy_from_slice(b"hello crc32c");
    Ext4DirEntryTail::new().to_disk_bytes(&mut block[tail_offset..tail_offset + 12]);

    update_ext4_dirblock_csum32(&sb, ino, generation, &mut block);

    let stored = u32::from_le_bytes(block[BLOCK_SIZE - 4..].try_into().unwrap());
    let expected = ext4_dirblock_csum32(&sb, ino, generation, &block[..tail_offset]);
    assert_eq!(stored, expected);
    assert!(verify_ext4_dirblock_checksum(&sb, ino, generation, &block));

    block[0] ^= 0x80;
    assert!(!verify_ext4_dirblock_checksum(&sb, ino, generation, &block));
}

#[test]
fn dx_checksum_uses_counted_entries_and_tail() {
    // Test idea: HTree index blocks store a dx_tail checksum after the full entry limit,
    // while the checksum covers only the counted dx entries plus the tail's reserved word.
    let sb = metadata_csum_superblock();
    let ino = 704258u32;
    let generation = 7817325u32;
    let mut block = [0u8; BLOCK_SIZE];
    let count_offset = 32;
    let entry_size = ::core::mem::size_of::<Ext4DxEntry>();
    let limit = ((BLOCK_SIZE - count_offset - 8) / entry_size) as u16;
    let count = 3u16;
    let tail_offset = count_offset + limit as usize * entry_size;

    block[0..4].copy_from_slice(&ino.to_le_bytes());
    block[4..6].copy_from_slice(&12u16.to_le_bytes());
    block[6] = 1;
    block[7] = 2;
    block[12..16].copy_from_slice(&2u32.to_le_bytes());
    block[16..18].copy_from_slice(&((BLOCK_SIZE - 12) as u16).to_le_bytes());
    block[18] = 2;
    block[19] = 2;
    block[24..28].fill(0);
    block[29] = 8;
    block[30] = 0;
    block[count_offset..count_offset + 2].copy_from_slice(&limit.to_le_bytes());
    block[count_offset + 2..count_offset + 4].copy_from_slice(&count.to_le_bytes());
    block[count_offset + 4..count_offset + 8].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    block[count_offset + 8..count_offset + 12].copy_from_slice(&1u32.to_le_bytes());
    block[count_offset + 12..count_offset + 16].copy_from_slice(&0x9ABC_DEF0u32.to_le_bytes());
    block[count_offset + 16..count_offset + 20].copy_from_slice(&5u32.to_le_bytes());
    block[tail_offset..tail_offset + 4].copy_from_slice(&0u32.to_le_bytes());

    let expected = ext4_metadata_csum32(
        ext4_crc32c_seed_from_superblock(&sb),
        &[
            &ino.to_le_bytes(),
            &generation.to_le_bytes(),
            &block[..count_offset + count as usize * entry_size],
            &block[tail_offset..tail_offset + 4],
            &[0, 0, 0, 0],
        ],
    );
    block[tail_offset + 4..tail_offset + 8].copy_from_slice(&expected.to_le_bytes());

    assert_eq!(
        verify_ext4_dx_checksum(&sb, ino, generation, &block),
        Some(true)
    );
    block[count_offset + 8] ^= 0x20;
    assert_eq!(
        verify_ext4_dx_checksum(&sb, ino, generation, &block),
        Some(false)
    );
}

#[test]
fn journal_superblock_checksum_uses_raw_crc_accumulator() {
    // Test idea: JBD2 stores the raw ext2fs_crc32c_le(~0, superblock) accumulator, not
    // the finalized CRC32C value. This keeps the value accepted by e2fsck.
    let mut jsb = JournalSuperBllockS {
        s_blocksize: BLOCK_SIZE as u32,
        s_maxlen: 8192,
        s_first: 1,
        s_sequence: 5,
        s_feature_incompat: JBD2_FEATURE_INCOMPAT_64BIT | JBD2_FEATURE_INCOMPAT_CSUM_V3,
        s_uuid: [
            0xFE, 0x1C, 0xE6, 0xEF, 0x04, 0xBB, 0x44, 0x44, 0x8F, 0x6E, 0x8D, 0x12, 0xBE, 0xE0,
            0x8A, 0xB7,
        ],
        s_nr_users: 1,
        s_checksum_type: JBD2_CRC32C_CHKSUM,
        ..Default::default()
    };
    jsb.s_padding[1] = 0x460;

    assert_eq!(jbd2_superblock_csum32(&jsb), 1091070733);

    jbd2_update_superblock_checksum(&mut jsb);
    assert_eq!(jsb.s_checksum, 1091070733);
}
