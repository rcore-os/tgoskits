//! CRC-focused integration tests for metadata integrity.
//!
//! These tests validate the `metadata_csum` behavior that protects ext4
//! metadata around normal file operations. They intentionally target
//! superblocks, group descriptors, and bitmaps after writing a file.
//! File payload blocks themselves are not covered because this implementation
//! does not currently expose a data-block CRC feature.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    rc::Rc,
};

use ax_kspin_test_runtime as _;
use rsext4::{
    blockgroup_description::Ext4GroupDesc,
    bmalloc::AbsoluteBN,
    checksum::{
        ext4_block_bitmap_csum32, ext4_group_desc_csum16, ext4_inode_bitmap_csum32,
        ext4_superblock_csum32,
    },
    endian::DiskFormat,
    error::{Errno, Ext4Error, Ext4Result},
    jbd2::jbdstruct::{
        JBD2_BLOCKTYPE_DESCRIPTOR, JBD2_BLOCKTYPE_REVOKE, JBD2_FLAG_LAST_TAG, JBD2_FLAG_SAME_UUID,
        JBD2_MAGIC, JBD2_UUID_SIZE, JournalBlockTagS, JournalHeaderS, JournalSuperBllockS,
    },
    loopfile::resolve_inode_block,
    superblock::Ext4Superblock,
    *,
};

/// Shared in-memory block device so tests can remount the same disk image and
/// corrupt raw metadata bytes between mounts without relying on private APIs.
#[derive(Clone)]
struct SharedCrcDevice {
    data: Rc<RefCell<Vec<u8>>>,
    block_size: u32,
    now: Rc<Cell<i64>>,
    blocked_read_block: Rc<Cell<Option<u64>>>,
}

impl SharedCrcDevice {
    fn new(size: usize) -> Self {
        Self {
            data: Rc::new(RefCell::new(vec![0; size])),
            block_size: BLOCK_SIZE as u32,
            now: Rc::new(Cell::new(1_700_000_000)),
            blocked_read_block: Rc::new(Cell::new(None)),
        }
    }

    fn read_bytes(&self, offset: usize, len: usize) -> Vec<u8> {
        self.data.borrow()[offset..offset + len].to_vec()
    }

    fn write_bytes(&self, offset: usize, bytes: &[u8]) {
        self.data.borrow_mut()[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn read_block_bytes(&self, block_id: u64) -> Vec<u8> {
        self.read_bytes(block_id as usize * BLOCK_SIZE, BLOCK_SIZE)
    }

    fn write_block_bytes(&self, block_id: u64, bytes: &[u8]) {
        self.write_bytes(block_id as usize * BLOCK_SIZE, bytes);
    }
}

impl BlockDevice for SharedCrcDevice {
    fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        if self.blocked_read_block.get() == Some(block_id.raw()) {
            return Err(Ext4Error::io());
        }
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.borrow().len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.borrow().len() / self.block_size as usize) as u64,
            ));
        }
        buffer.copy_from_slice(&self.data.borrow()[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
        let start = block_id.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.borrow().len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.borrow().len() / self.block_size as usize) as u64,
            ));
        }
        self.data.borrow_mut()[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn open(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn total_blocks(&self) -> u64 {
        (self.data.borrow().len() / self.block_size as usize) as u64
    }

    fn block_size(&self) -> u32 {
        self.block_size
    }

    fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn new_jbd2_dev(device: SharedCrcDevice) -> Jbd2Dev<SharedCrcDevice> {
    Jbd2Dev::initial_jbd2dev(0, device, true)
}

fn build_filesystem_with_written_file() -> (SharedCrcDevice, Vec<u8>) {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let payload = b"crc integration payload".to_vec();

    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut fs = mount(&mut jbd2_dev).expect("mount failed");
    mkfile(&mut jbd2_dev, &mut fs, "/crc.txt", Some(&payload), None).expect("mkfile failed");
    umount(fs, &mut jbd2_dev).expect("umount failed");

    (device, payload)
}

fn sync_with_axfs_ng_order(
    dev: &mut Jbd2Dev<SharedCrcDevice>,
    fs: &mut Ext4FileSystem,
) -> Ext4Result<()> {
    fs.datablock_cache.flush_all(dev)?;
    fs.bitmap_cache.flush_all(dev)?;
    fs.inodetable_cache.flush_all(dev)?;
    fs.superblock.s_state = Ext4Superblock::EXT4_VALID_FS;
    fs.sync_superblock(dev)?;
    fs.sync_group_descriptors(dev)?;
    if dev.is_use_journal() {
        dev.umount_commit();
    }
    dev.cantflush()
}

fn read_superblock(device: &SharedCrcDevice) -> Ext4Superblock {
    let bytes = device.read_bytes(SUPERBLOCK_OFFSET as usize, Ext4Superblock::SUPERBLOCK_SIZE);
    Ext4Superblock::from_disk_bytes(&bytes)
}

fn write_superblock(device: &SharedCrcDevice, sb: &Ext4Superblock) {
    let mut bytes = vec![0u8; Ext4Superblock::SUPERBLOCK_SIZE];
    sb.to_disk_bytes(&mut bytes);
    device.write_bytes(SUPERBLOCK_OFFSET as usize, &bytes);
}

fn read_group_desc0(device: &SharedCrcDevice, sb: &Ext4Superblock) -> Ext4GroupDesc {
    let desc_size = sb.get_desc_size() as usize;
    let bytes = device.read_bytes(BLOCK_SIZE, desc_size);
    Ext4GroupDesc::from_disk_bytes(&bytes)
}

fn write_group_desc0(device: &SharedCrcDevice, sb: &Ext4Superblock, desc: &Ext4GroupDesc) {
    let desc_size = sb.get_desc_size() as usize;
    let mut bytes = vec![0u8; Ext4GroupDesc::EXT4_DESC_SIZE_64BIT];
    desc.to_disk_bytes(&mut bytes);
    device.write_bytes(BLOCK_SIZE, &bytes[..desc_size]);
}

fn write_journal_start(device: &SharedCrcDevice, journal_block: u64, start: u32) {
    let mut bytes = device.read_block_bytes(journal_block);
    let mut journal_sb = JournalSuperBllockS::from_disk_bytes(&bytes);
    journal_sb.s_start = start;
    journal_sb.to_disk_bytes(&mut bytes);
    device.write_block_bytes(journal_block, &bytes);
}

fn write_incomplete_journal_descriptor(device: &SharedCrcDevice, journal_block: u64) {
    let bytes = device.read_block_bytes(journal_block);
    let journal_sb = JournalSuperBllockS::from_disk_bytes(&bytes);

    let mut descriptor = vec![0u8; BLOCK_SIZE];
    JournalHeaderS {
        h_magic: JBD2_MAGIC,
        h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
        h_sequence: journal_sb.s_sequence,
    }
    .to_disk_bytes(&mut descriptor);
    device.write_block_bytes(journal_block + 1, &descriptor);
}

fn write_uncommitted_journal_update(
    device: &SharedCrcDevice,
    journal_block: u64,
    target_block: u64,
    payload: &[u8],
) {
    let bytes = device.read_block_bytes(journal_block);
    let journal_sb = JournalSuperBllockS::from_disk_bytes(&bytes);

    let mut descriptor = vec![0u8; BLOCK_SIZE];
    JournalHeaderS {
        h_magic: JBD2_MAGIC,
        h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
        h_sequence: journal_sb.s_sequence,
    }
    .to_disk_bytes(&mut descriptor);
    JournalBlockTagS {
        t_blocknr: target_block as u32,
        t_checksum: 0,
        t_flags: JBD2_FLAG_LAST_TAG,
    }
    .to_disk_bytes(&mut descriptor[12..20]);
    descriptor[20..20 + JBD2_UUID_SIZE].copy_from_slice(&journal_sb.s_uuid);
    device.write_block_bytes(journal_block + 1, &descriptor);

    let mut metadata = vec![0u8; BLOCK_SIZE];
    metadata[..payload.len()].copy_from_slice(payload);
    device.write_block_bytes(journal_block + 2, &metadata);
}

fn write_invalid_journal_revoke(device: &SharedCrcDevice, journal_block: u64) {
    let bytes = device.read_block_bytes(journal_block);
    let journal_sb = JournalSuperBllockS::from_disk_bytes(&bytes);

    let mut revoke = vec![0u8; BLOCK_SIZE];
    JournalHeaderS {
        h_magic: JBD2_MAGIC,
        h_blocktype: JBD2_BLOCKTYPE_REVOKE,
        h_sequence: journal_sb.s_sequence,
    }
    .to_disk_bytes(&mut revoke);
    revoke[12..16].copy_from_slice(&((BLOCK_SIZE as u32) + 1).to_be_bytes());
    device.write_block_bytes(journal_block + 1, &revoke);
}

fn write_repeating_journal_descriptors(device: &SharedCrcDevice, journal_block: u64) {
    let bytes = device.read_block_bytes(journal_block);
    let journal_sb = JournalSuperBllockS::from_disk_bytes(&bytes);

    let mut descriptor = vec![0u8; BLOCK_SIZE];
    JournalHeaderS {
        h_magic: JBD2_MAGIC,
        h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
        h_sequence: journal_sb.s_sequence,
    }
    .to_disk_bytes(&mut descriptor);
    JournalBlockTagS {
        t_blocknr: (journal_block - 1) as u32,
        t_checksum: 0,
        t_flags: JBD2_FLAG_LAST_TAG,
    }
    .to_disk_bytes(&mut descriptor[12..20]);
    descriptor[20..20 + JBD2_UUID_SIZE].copy_from_slice(&journal_sb.s_uuid);

    for rel in journal_sb.s_first..journal_sb.s_maxlen {
        device.write_block_bytes(journal_block + u64::from(rel), &descriptor);
    }
}

fn write_uncommitted_journal_updates(
    device: &SharedCrcDevice,
    journal_block: u64,
    target_blocks: &[u64],
) {
    let bytes = device.read_block_bytes(journal_block);
    let journal_sb = JournalSuperBllockS::from_disk_bytes(&bytes);

    let mut descriptor = vec![0u8; BLOCK_SIZE];
    JournalHeaderS {
        h_magic: JBD2_MAGIC,
        h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
        h_sequence: journal_sb.s_sequence,
    }
    .to_disk_bytes(&mut descriptor);

    let mut offset = 12usize;
    for (idx, target) in target_blocks.iter().enumerate() {
        let mut flags = 0;
        if idx > 0 {
            flags |= JBD2_FLAG_SAME_UUID;
        }
        if idx == target_blocks.len() - 1 {
            flags |= JBD2_FLAG_LAST_TAG;
        }
        JournalBlockTagS {
            t_blocknr: *target as u32,
            t_checksum: 0,
            t_flags: flags,
        }
        .to_disk_bytes(&mut descriptor[offset..offset + 8]);
        offset += 8;
        if idx == 0 {
            descriptor[offset..offset + JBD2_UUID_SIZE].copy_from_slice(&journal_sb.s_uuid);
            offset += JBD2_UUID_SIZE;
        }
    }
    device.write_block_bytes(journal_block + 1, &descriptor);

    for (idx, _) in target_blocks.iter().enumerate() {
        let mut metadata = vec![0u8; BLOCK_SIZE];
        metadata[..8].copy_from_slice(&(idx as u64).to_le_bytes());
        device.write_block_bytes(journal_block + 2 + idx as u64, &metadata);
    }
}

#[test]
fn checksums_are_persisted_and_clean_remount_preserves_the_written_file() {
    // Test idea: write one real file, inspect the raw on-disk checksum fields,
    // and then remount to prove the intact image passes verification end to end.
    let (device, payload) = build_filesystem_with_written_file();

    let sb = read_superblock(&device);
    assert!(sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM));
    assert_ne!(sb.s_checksum, 0);
    assert_eq!(sb.s_checksum, ext4_superblock_csum32(&sb));

    let desc = read_group_desc0(&device, &sb);
    let mut desc_for_csum = desc;
    desc_for_csum.bg_checksum = 0;
    let mut desc_bytes = [0u8; Ext4GroupDesc::EXT4_DESC_SIZE_64BIT];
    desc_for_csum.to_disk_bytes(&mut desc_bytes);
    let expected_desc_csum =
        ext4_group_desc_csum16(&sb, 0, &desc_bytes[..sb.get_desc_size() as usize]);
    assert_eq!(desc.bg_checksum, expected_desc_csum);

    let block_bitmap = device.read_block_bytes(desc.block_bitmap());
    let inode_bitmap = device.read_block_bytes(desc.inode_bitmap());
    assert_eq!(
        desc.block_bitmap_csum(&sb),
        ext4_block_bitmap_csum32(&sb, &block_bitmap)
    );
    assert_eq!(
        desc.inode_bitmap_csum(&sb),
        ext4_inode_bitmap_csum32(&sb, &inode_bitmap)
    );

    let mut remount_dev = new_jbd2_dev(device.clone());
    let mut fs = mount(&mut remount_dev).expect("mount after intact checksum data failed");
    let read_back = read_file(&mut remount_dev, &mut fs, "/crc.txt").expect("read_file failed");
    assert_eq!(read_back, payload);
    umount(fs, &mut remount_dev).expect("umount failed");
}

#[test]
fn axfs_ng_sync_order_preserves_inode_bitmap_across_remount() {
    // Test idea: mirror axfs-ng's sync_to_disk ordering, then remount and keep
    // creating files. Inodes allocated before the sync must remain marked in
    // the persisted inode bitmap and must not be reused after remount.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut first_dev = new_jbd2_dev(device.clone());
    mkfs(&mut first_dev).expect("mkfs failed");
    let mut fs = mount(&mut first_dev).expect("mount failed");

    let mut seen = BTreeSet::new();
    for idx in 0..256 {
        let path = format!("/before-{idx}");
        mkfile(&mut first_dev, &mut fs, &path, Some(b"x"), None).expect("mkfile before failed");
        let file = open(&mut first_dev, &mut fs, &path, false).expect("open before failed");
        assert!(
            seen.insert(file.inode_num.raw()),
            "duplicate inode before sync"
        );
    }

    sync_with_axfs_ng_order(&mut first_dev, &mut fs).expect("axfs-ng order sync failed");
    drop(fs);
    drop(first_dev);

    let sb = read_superblock(&device);
    let desc = read_group_desc0(&device, &sb);
    let inode_bitmap = device.read_block_bytes(desc.inode_bitmap());
    assert_eq!(
        desc.inode_bitmap_csum(&sb),
        ext4_inode_bitmap_csum32(&sb, &inode_bitmap)
    );

    let mut remount_dev = new_jbd2_dev(device.clone());
    let mut fs = mount(&mut remount_dev).expect("mount after axfs-ng order sync failed");

    for idx in 0..256 {
        let path = format!("/after-{idx}");
        mkfile(&mut remount_dev, &mut fs, &path, Some(b"y"), None).expect("mkfile after failed");
        let file = open(&mut remount_dev, &mut fs, &path, false).expect("open after failed");
        assert!(
            seen.insert(file.inode_num.raw()),
            "inode reused after axfs-ng order sync/remount"
        );
    }

    umount(fs, &mut remount_dev).expect("umount failed");
}

#[test]
fn old_32_byte_descriptors_match_low_16_bits_of_bitmap_checksums() {
    let (device, _payload) = build_filesystem_with_written_file();
    let mut sb = read_superblock(&device);
    let mut desc = read_group_desc0(&device, &sb);

    sb.s_feature_incompat &= !Ext4Superblock::EXT4_FEATURE_INCOMPAT_64BIT;
    sb.s_desc_size = Ext4GroupDesc::GOOD_OLD_DESC_SIZE as u16;
    desc.bg_block_bitmap_csum_hi = 0;
    desc.bg_inode_bitmap_csum_hi = 0;

    let block_bitmap = device.read_block_bytes(desc.block_bitmap());
    let inode_bitmap = device.read_block_bytes(desc.inode_bitmap());
    let block_csum = ext4_block_bitmap_csum32(&sb, &block_bitmap);
    let inode_csum = ext4_inode_bitmap_csum32(&sb, &inode_bitmap);

    desc.bg_block_bitmap_csum_lo = block_csum as u16;
    desc.bg_inode_bitmap_csum_lo = inode_csum as u16;

    assert!(desc.block_bitmap_csum_matches(&sb, block_csum));
    assert!(desc.inode_bitmap_csum_matches(&sb, inode_csum));
    assert!(!desc.block_bitmap_csum_matches(&sb, block_csum ^ 1));
    assert!(!desc.inode_bitmap_csum_matches(&sb, inode_csum ^ 1));
}

#[test]
fn incomplete_journal_is_not_replayed_when_recovery_flag_is_clear() {
    // Test idea: ext4 recovery is driven by the superblock needs_recovery bit,
    // not by leftover journal state. If we clear that bit on disk and leave a
    // deliberately broken journal descriptor behind, the next mount must
    // ignore the journal contents instead of trying to replay them. The mount
    // itself will still set needs_recovery for its own writable session, and a
    // clean umount must clear it again before the test ends.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut first_mount_dev).expect("mount failed");
    let journal_block = fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    umount(fs, &mut first_mount_dev).expect("umount failed");

    let mut sb = read_superblock(&device);
    sb.s_feature_incompat &= !Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
    sb.update_checksum();
    write_superblock(&device, &sb);
    write_journal_start(&device, journal_block, 1);
    write_incomplete_journal_descriptor(&device, journal_block);

    let clean_mount_sb = read_superblock(&device);
    assert_eq!(
        clean_mount_sb.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );

    let mut remount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut remount_dev).expect("clean mount should not force journal replay");
    assert_ne!(
        fs.superblock.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );
    assert!(remount_dev.is_use_journal());
    umount(fs, &mut remount_dev).expect("umount failed");

    let clean_unmount_sb = read_superblock(&device);
    assert_eq!(
        clean_unmount_sb.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );
    assert_ne!(clean_unmount_sb.s_lpf_ino, 0);
}

#[test]
fn uncommitted_journal_tail_is_discarded_during_recovery() {
    // Test idea: an unclean shutdown may leave a descriptor for a transaction
    // that never reached its commit block. The transaction is not durable, so
    // recovery must discard the tail instead of failing the whole mount.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut first_mount_dev).expect("mount failed");
    let journal_block = fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    umount(fs, &mut first_mount_dev).expect("umount failed");

    let mut sb = read_superblock(&device);
    sb.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
    sb.update_checksum();
    write_superblock(&device, &sb);
    let target_block = journal_block - 1;
    let original_target = device.read_block_bytes(target_block);
    write_journal_start(&device, journal_block, 1);
    write_uncommitted_journal_update(
        &device,
        journal_block,
        target_block,
        b"uncommitted metadata payload",
    );

    let mut remount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut remount_dev).expect("mount should discard uncommitted journal tail");
    assert_eq!(
        fs.superblock.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );
    umount(fs, &mut remount_dev).expect("umount failed");

    assert_eq!(device.read_block_bytes(target_block), original_target);

    let recovered_journal = device.read_block_bytes(journal_block);
    let recovered_journal_sb = JournalSuperBllockS::from_disk_bytes(&recovered_journal);
    assert_eq!(recovered_journal_sb.s_start, 0);
}

#[test]
fn uncommitted_journal_tail_does_not_read_payload_blocks() {
    // Test idea: Linux JBD2 first scans control records to find a commit block.
    // Payload blocks from an uncommitted transaction must not be read during recovery.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut first_mount_dev).expect("mount failed");
    let journal_block = fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    umount(fs, &mut first_mount_dev).expect("umount failed");

    let mut sb = read_superblock(&device);
    sb.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
    sb.update_checksum();
    write_superblock(&device, &sb);
    write_journal_start(&device, journal_block, 1);
    write_uncommitted_journal_updates(
        &device,
        journal_block,
        &[journal_block - 1, journal_block - 2],
    );
    device.blocked_read_block.set(Some(journal_block + 2));

    let mut remount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut remount_dev).expect("uncommitted payload should not be read");
    assert_eq!(
        fs.superblock.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );
    umount(fs, &mut remount_dev).expect("umount failed");
}

#[test]
fn invalid_revoke_record_fails_recovery() {
    // Test idea: Linux JBD2 treats an expected-sequence revoke block with an
    // invalid record count as journal corruption, not as an uncommitted tail.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut first_mount_dev).expect("mount failed");
    let journal_block = fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    umount(fs, &mut first_mount_dev).expect("umount failed");

    let mut sb = read_superblock(&device);
    sb.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
    sb.update_checksum();
    write_superblock(&device, &sb);
    write_journal_start(&device, journal_block, 1);
    write_invalid_journal_revoke(&device, journal_block);

    let mut remount_dev = new_jbd2_dev(device);
    let err = match mount(&mut remount_dev) {
        Ok(_) => panic!("invalid revoke block should fail recovery"),
        Err(err) => err,
    };
    assert_eq!(err.code, Errno::EUCLEAN);
}

#[test]
fn readonly_no_replay_mount_can_inspect_unrecoverable_journal() {
    // Test idea: callers that only need to inspect or read files may explicitly
    // choose a read-only mount without journal replay. The default writable
    // mount must still reject the same image because home metadata may be stale.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut first_mount_dev).expect("mount failed");
    let journal_block = fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    umount(fs, &mut first_mount_dev).expect("umount failed");

    let mut sb = read_superblock(&device);
    sb.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
    sb.update_checksum();
    write_superblock(&device, &sb);
    write_journal_start(&device, journal_block, 1);
    write_invalid_journal_revoke(&device, journal_block);

    let mut writable_dev = new_jbd2_dev(device.clone());
    let err = match mount(&mut writable_dev) {
        Ok(_) => panic!("default mount should fail unrecoverable journal replay"),
        Err(err) => err,
    };
    assert_eq!(err.code, Errno::EUCLEAN);

    let mut readonly_dev = Jbd2Dev::initial_jbd2dev(0, device.clone(), false);
    let fs = mount_with_options(
        &mut readonly_dev,
        MountOptions::read_only_no_journal_replay(),
    )
    .expect("read-only no-replay mount should allow inspection");
    assert_ne!(
        fs.superblock.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );
    assert!(!readonly_dev.is_use_journal());

    let on_disk_sb = read_superblock(&device);
    assert_ne!(
        on_disk_sb.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );
}

#[test]
fn empty_descriptor_header_is_discarded_during_recovery() {
    // Test idea: a crash can leave only the descriptor header without any tags.
    // With no commit block, this is an uncommitted tail rather than durable work.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut first_mount_dev).expect("mount failed");
    let journal_block = fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    umount(fs, &mut first_mount_dev).expect("umount failed");

    let mut sb = read_superblock(&device);
    sb.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
    sb.update_checksum();
    write_superblock(&device, &sb);
    write_journal_start(&device, journal_block, 1);
    write_incomplete_journal_descriptor(&device, journal_block);

    let mut remount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut remount_dev).expect("mount should discard empty descriptor tail");
    assert_eq!(
        fs.superblock.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );
    umount(fs, &mut remount_dev).expect("umount failed");
}

#[test]
fn replay_scan_is_bounded_by_journal_ring_length() {
    // Test idea: malformed journal contents that keep looking like the expected
    // sequence must not make recovery loop forever around the journal ring.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut first_mount_dev).expect("mount failed");
    let journal_block = fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    umount(fs, &mut first_mount_dev).expect("umount failed");

    let mut sb = read_superblock(&device);
    sb.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
    sb.update_checksum();
    write_superblock(&device, &sb);
    write_journal_start(&device, journal_block, 1);
    write_repeating_journal_descriptors(&device, journal_block);

    let mut remount_dev = new_jbd2_dev(device);
    let err = match mount(&mut remount_dev) {
        Ok(_) => panic!("cyclic journal scan should fail recovery"),
        Err(err) => err,
    };
    assert_eq!(err.code, Errno::EUCLEAN);
}

#[test]
fn path_resolved_lost_found_rebuilds_superblock_hint() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let clean_sb = read_superblock(&device);
    assert_ne!(clean_sb.s_lpf_ino, 0);

    let mut missing_hint = clean_sb;
    missing_hint.s_lpf_ino = 0;
    missing_hint.update_checksum();
    write_superblock(&device, &missing_hint);

    let mut remount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut remount_dev).expect("mount should resolve existing lost+found");
    assert_ne!(fs.superblock.s_lpf_ino, 0);
    assert_eq!(fs.superblock.s_lpf_ino, clean_sb.s_lpf_ino);
    umount(fs, &mut remount_dev).expect("umount failed");

    let repaired_sb = read_superblock(&device);
    assert_eq!(repaired_sb.s_lpf_ino, clean_sb.s_lpf_ino);
}

#[test]
fn mount_uses_valid_lost_found_hint_without_root_path_scan() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut inspect_dev = new_jbd2_dev(device.clone());
    let mut fs = mount(&mut inspect_dev).expect("mount failed");
    let mut root = fs.get_root(&mut inspect_dev).expect("root inode");
    let root_block = resolve_inode_block(&mut inspect_dev, &mut root, 0)
        .expect("resolve root block")
        .expect("root directory block")
        .raw();
    umount(fs, &mut inspect_dev).expect("umount failed");

    let clean_sb = read_superblock(&device);
    assert_ne!(clean_sb.s_lpf_ino, 0);

    device.blocked_read_block.set(Some(root_block));
    let mut remount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut remount_dev).expect("mount should trust valid lost+found hint");
    assert_eq!(fs.superblock.s_lpf_ino, clean_sb.s_lpf_ino);
}

#[test]
fn unclean_shutdown_mount_state_does_not_set_error_fs() {
    // Test idea: a crash after mount should leave the filesystem unclean, but
    // it must not be reported as EXT4_ERROR_FS on the next boot.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    {
        let mut fs = mount(&mut jbd2_dev).expect("mount failed");
        fs.sync_superblock(&mut jbd2_dev)
            .expect("persist dirty mount state");
    }

    let dirty_sb = read_superblock(&device);
    assert_eq!(dirty_sb.s_state & Ext4Superblock::EXT4_VALID_FS, 0);
    assert_eq!(dirty_sb.s_state & Ext4Superblock::EXT4_ERROR_FS, 0);

    let mut remount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut remount_dev).expect("mount after unclean shutdown failed");
    assert_eq!(fs.superblock.s_state & Ext4Superblock::EXT4_ERROR_FS, 0);
    umount(fs, &mut remount_dev).expect("umount failed");

    let clean_sb = read_superblock(&device);
    assert_eq!(clean_sb.s_state, Ext4Superblock::EXT4_VALID_FS);
}

#[test]
fn clean_unmount_preserves_real_error_fs_state() {
    // Test idea: EXT4_ERROR_FS is an independent state bit. A clean unmount may
    // mark the filesystem clean, but must not erase a recorded error.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut sb = read_superblock(&device);
    sb.s_state = Ext4Superblock::EXT4_VALID_FS | Ext4Superblock::EXT4_ERROR_FS;
    sb.s_error_count = 1;
    sb.update_checksum();
    write_superblock(&device, &sb);

    let mut remount_dev = new_jbd2_dev(device.clone());
    let fs = mount(&mut remount_dev).expect("mount with error state failed");
    assert_ne!(fs.superblock.s_state & Ext4Superblock::EXT4_ERROR_FS, 0);
    umount(fs, &mut remount_dev).expect("umount failed");

    let clean_sb = read_superblock(&device);
    assert_ne!(clean_sb.s_state & Ext4Superblock::EXT4_VALID_FS, 0);
    assert_ne!(clean_sb.s_state & Ext4Superblock::EXT4_ERROR_FS, 0);
}

#[test]
fn needs_recovery_enables_mount_replay_when_caller_disabled_journal() {
    // Test idea: EXT4_FEATURE_INCOMPAT_RECOVER means home metadata may be
    // stale. Mount should replay the journal before ordinary metadata access
    // even if the caller disabled journaling for normal writes.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut sb = read_superblock(&device);
    sb.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
    sb.update_checksum();
    write_superblock(&device, &sb);

    let mut remount_dev = Jbd2Dev::initial_jbd2dev(0, device.clone(), false);
    let fs = mount(&mut remount_dev).expect("mount should replay needs_recovery journal");
    assert!(!remount_dev.is_use_journal());
    assert_eq!(
        fs.superblock.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );

    let recovered_sb = read_superblock(&device);
    assert_eq!(
        recovered_sb.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );
}

#[test]
fn corrupted_superblock_checksum_is_reported_as_euclean_on_mount() {
    // Test idea: corrupt only the stored superblock CRC field and ensure mount
    // rejects the image with the checksum-specific EUCLEAN errno.
    let (device, _) = build_filesystem_with_written_file();

    let mut sb = read_superblock(&device);
    sb.s_checksum ^= 0x1;
    write_superblock(&device, &sb);

    let mut remount_dev = new_jbd2_dev(device);
    let err = match mount(&mut remount_dev) {
        Ok(_) => panic!("mount should fail on corrupted superblock CRC"),
        Err(err) => err,
    };
    assert_eq!(err.code, Errno::EUCLEAN);
}

#[test]
fn corrupted_group_descriptor_checksum_is_reported_as_euclean_on_mount() {
    // Test idea: corrupt the stored group descriptor checksum field and ensure
    // the descriptor verifier fails before mount starts normal filesystem work.
    let (device, _) = build_filesystem_with_written_file();

    let sb = read_superblock(&device);
    let mut desc = read_group_desc0(&device, &sb);
    desc.bg_checksum ^= 0x1;
    write_group_desc0(&device, &sb, &desc);

    let mut remount_dev = new_jbd2_dev(device);
    let err = match mount(&mut remount_dev) {
        Ok(_) => panic!("mount should fail on corrupted GDT CRC"),
        Err(err) => err,
    };
    assert_eq!(err.code, Errno::EUCLEAN);
}

#[test]
fn corrupted_block_bitmap_payload_is_reported_as_euclean_on_mount() {
    // Test idea: damage the protected bitmap payload while keeping the stored
    // checksum untouched so mount must discover the mismatch itself.
    let (device, _) = build_filesystem_with_written_file();

    let sb = read_superblock(&device);
    let desc = read_group_desc0(&device, &sb);
    let mut block_bitmap = device.read_block_bytes(desc.block_bitmap());
    block_bitmap[0] ^= 0x1;
    device.write_block_bytes(desc.block_bitmap(), &block_bitmap);

    let mut remount_dev = new_jbd2_dev(device);
    let err = match mount(&mut remount_dev) {
        Ok(_) => panic!("mount should fail on corrupted bitmap payload"),
        Err(err) => err,
    };
    assert_eq!(err.code, Errno::EUCLEAN);
}
