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

use rsext4::{
    blockgroup_description::Ext4GroupDesc,
    bmalloc::{AbsoluteBN, InodeNumber},
    checksum::{
        ext4_block_bitmap_csum32, ext4_group_desc_csum16, ext4_inode_bitmap_csum32,
        ext4_superblock_csum32, jbd2_update_superblock_checksum,
    },
    disknode::Ext4Inode,
    endian::DiskFormat,
    error::{Ext4Error, Ext4Result},
    jbd2::jbdstruct::{
        JBD2_BLOCKTYPE_DESCRIPTOR, JBD2_BLOCKTYPE_REVOKE, JBD2_BLOCKTYPE_SUPERBLOCK_V1,
        JBD2_CRC32C_CHKSUM, JBD2_FEATURE_INCOMPAT_64BIT, JBD2_FEATURE_INCOMPAT_CSUM_V3,
        JBD2_FLAG_LAST_TAG, JBD2_FLAG_SAME_UUID, JBD2_MAGIC, JBD2_UUID_SIZE, JOURNAL_FILE_INODE,
        JournalBlockTag3S, JournalBlockTagS, JournalHeaderS, JournalSuperBlock,
    },
    loopfile::{get_file_inode, resolve_inode_block, resolve_inode_blocks},
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
    fail_writes: Rc<Cell<bool>>,
    failing_write_block: Rc<Cell<Option<u64>>>,
    journal_superblock_write_attempts: Rc<Cell<u8>>,
    failing_write_attempts: Rc<RefCell<BTreeSet<u8>>>,
}

impl SharedCrcDevice {
    fn new(size: usize) -> Self {
        Self {
            data: Rc::new(RefCell::new(vec![0; size])),
            block_size: BLOCK_SIZE as u32,
            now: Rc::new(Cell::new(1_700_000_000)),
            blocked_read_block: Rc::new(Cell::new(None)),
            fail_writes: Rc::new(Cell::new(false)),
            failing_write_block: Rc::new(Cell::new(None)),
            journal_superblock_write_attempts: Rc::new(Cell::new(0)),
            failing_write_attempts: Rc::new(RefCell::new(BTreeSet::new())),
        }
    }

    fn read_bytes(&self, offset: usize, len: usize) -> Vec<u8> {
        self.data.borrow()[offset..offset + len].to_vec()
    }

    fn write_bytes(&self, offset: usize, bytes: &[u8]) {
        self.data.borrow_mut()[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn read_block_bytes(&self, sector: u64) -> Vec<u8> {
        self.read_bytes(sector as usize * BLOCK_SIZE, BLOCK_SIZE)
    }

    fn write_block_bytes(&self, sector: u64, bytes: &[u8]) {
        self.write_bytes(sector as usize * BLOCK_SIZE, bytes);
    }
}

impl BlockIo for SharedCrcDevice {
    fn read(&mut self, buffer: &mut [u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        if self.blocked_read_block.get() == Some(sector.raw()) {
            return Err(Ext4Error::io());
        }
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.borrow().len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                (self.data.borrow().len() / self.block_size as usize) as u64,
            ));
        }
        buffer.copy_from_slice(&self.data.borrow()[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        if self.failing_write_block.get() == Some(sector.raw()) {
            let attempt = self.journal_superblock_write_attempts.get() + 1;
            self.journal_superblock_write_attempts.set(attempt);
            if self.failing_write_attempts.borrow_mut().remove(&attempt) {
                return Err(Ext4Error::io());
            }
        }
        if self.fail_writes.get() {
            return Err(Ext4Error::io());
        }
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.borrow().len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                (self.data.borrow().len() / self.block_size as usize) as u64,
            ));
        }
        self.data.borrow_mut()[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn geometry(&self) -> rsext4::DeviceGeometry {
        rsext4::DeviceGeometry::new(self.block_size, {
            (self.data.borrow().len() / self.block_size as usize) as u64
        })
    }

    fn capabilities(&self) -> rsext4::DeviceCapabilities {
        rsext4::DeviceCapabilities {
            read_only: { false },

            flush: true,

            ..rsext4::DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> rsext4::Ext4Result<()> {
        Ok(())
    }
}

impl rsext4::Clock for SharedCrcDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn new_jbd2_dev(device: SharedCrcDevice) -> Jbd2Dev<SharedCrcDevice> {
    Jbd2Dev::initial_jbd2dev(0, device, true)
}

#[derive(Clone, Copy)]
struct OwnedTestClock;

impl Clock for OwnedTestClock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::new(1_700_000_000, 0))
    }
}

fn build_filesystem_with_written_file() -> (SharedCrcDevice, Vec<u8>) {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let payload = b"crc integration payload".to_vec();

    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
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
        dev.umount_commit()?;
    }
    dev.flush()
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
    let mut journal_sb = JournalSuperBlock::from_disk_bytes(&bytes);
    journal_sb.s_start = start;
    jbd2_update_superblock_checksum(&mut journal_sb);
    journal_sb.to_disk_bytes(&mut bytes);
    device.write_block_bytes(journal_block, &bytes);
}

fn reference_crc32c(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82f6_3b78
            } else {
                crc >> 1
            };
        }
    }
    crc
}

fn jbd2_tag_checksum(journal_sb: &JournalSuperBlock, payload: &[u8]) -> u32 {
    let checksum = reference_crc32c(u32::MAX, &journal_sb.s_uuid);
    let checksum = reference_crc32c(checksum, &journal_sb.s_sequence.to_be_bytes());
    reference_crc32c(checksum, payload)
}

fn seal_jbd2_control_block(journal_sb: &JournalSuperBlock, block: &mut [u8]) {
    if journal_sb.s_feature_incompat & JBD2_FEATURE_INCOMPAT_CSUM_V3 == 0 {
        return;
    }
    let checksum_offset = block.len() - 4;
    block[checksum_offset..].fill(0);
    let checksum = reference_crc32c(u32::MAX, &journal_sb.s_uuid);
    let checksum = reference_crc32c(checksum, block);
    block[checksum_offset..].copy_from_slice(&checksum.to_be_bytes());
}

fn write_incomplete_journal_descriptor(device: &SharedCrcDevice, journal_block: u64) {
    let bytes = device.read_block_bytes(journal_block);
    let journal_sb = JournalSuperBlock::from_disk_bytes(&bytes);

    let mut descriptor = vec![0u8; BLOCK_SIZE];
    JournalHeaderS {
        h_magic: JBD2_MAGIC,
        h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
        h_sequence: journal_sb.s_sequence,
    }
    .to_disk_bytes(&mut descriptor);
    seal_jbd2_control_block(&journal_sb, &mut descriptor);
    device.write_block_bytes(journal_block + 1, &descriptor);
}

fn write_uncommitted_journal_update(
    device: &SharedCrcDevice,
    journal_block: u64,
    target_block: u64,
    payload: &[u8],
) {
    let bytes = device.read_block_bytes(journal_block);
    let journal_sb = JournalSuperBlock::from_disk_bytes(&bytes);

    let mut metadata = vec![0u8; BLOCK_SIZE];
    metadata[..payload.len()].copy_from_slice(payload);

    let mut descriptor = vec![0u8; BLOCK_SIZE];
    JournalHeaderS {
        h_magic: JBD2_MAGIC,
        h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
        h_sequence: journal_sb.s_sequence,
    }
    .to_disk_bytes(&mut descriptor);
    if journal_sb.s_feature_incompat & JBD2_FEATURE_INCOMPAT_CSUM_V3 != 0 {
        JournalBlockTag3S {
            t_blocknr: target_block as u32,
            t_flags: u32::from(JBD2_FLAG_LAST_TAG),
            t_blocknr_high: (target_block >> 32) as u32,
            t_checksum: jbd2_tag_checksum(&journal_sb, &metadata),
        }
        .to_disk_bytes(&mut descriptor[12..28]);
        descriptor[28..28 + JBD2_UUID_SIZE].copy_from_slice(&journal_sb.s_uuid);
    } else {
        JournalBlockTagS {
            t_blocknr: target_block as u32,
            t_checksum: 0,
            t_flags: JBD2_FLAG_LAST_TAG,
        }
        .to_disk_bytes(&mut descriptor[12..20]);
        descriptor[20..20 + JBD2_UUID_SIZE].copy_from_slice(&journal_sb.s_uuid);
    }
    seal_jbd2_control_block(&journal_sb, &mut descriptor);
    device.write_block_bytes(journal_block + 1, &descriptor);
    device.write_block_bytes(journal_block + 2, &metadata);
}

fn write_invalid_journal_revoke(device: &SharedCrcDevice, journal_block: u64) {
    let bytes = device.read_block_bytes(journal_block);
    let journal_sb = JournalSuperBlock::from_disk_bytes(&bytes);

    let mut revoke = vec![0u8; BLOCK_SIZE];
    JournalHeaderS {
        h_magic: JBD2_MAGIC,
        h_blocktype: JBD2_BLOCKTYPE_REVOKE,
        h_sequence: journal_sb.s_sequence,
    }
    .to_disk_bytes(&mut revoke);
    revoke[12..16].copy_from_slice(&((BLOCK_SIZE as u32) + 1).to_be_bytes());
    seal_jbd2_control_block(&journal_sb, &mut revoke);
    device.write_block_bytes(journal_block + 1, &revoke);
}

fn write_repeating_journal_descriptors(device: &SharedCrcDevice, journal_block: u64) {
    let bytes = device.read_block_bytes(journal_block);
    let journal_sb = JournalSuperBlock::from_disk_bytes(&bytes);

    let mut descriptor = vec![0u8; BLOCK_SIZE];
    JournalHeaderS {
        h_magic: JBD2_MAGIC,
        h_blocktype: JBD2_BLOCKTYPE_DESCRIPTOR,
        h_sequence: journal_sb.s_sequence,
    }
    .to_disk_bytes(&mut descriptor);
    if journal_sb.s_feature_incompat & JBD2_FEATURE_INCOMPAT_CSUM_V3 != 0 {
        JournalBlockTag3S {
            t_blocknr: (journal_block - 1) as u32,
            t_flags: u32::from(JBD2_FLAG_LAST_TAG),
            t_blocknr_high: ((journal_block - 1) >> 32) as u32,
            t_checksum: 0,
        }
        .to_disk_bytes(&mut descriptor[12..28]);
        descriptor[28..28 + JBD2_UUID_SIZE].copy_from_slice(&journal_sb.s_uuid);
    } else {
        JournalBlockTagS {
            t_blocknr: (journal_block - 1) as u32,
            t_checksum: 0,
            t_flags: JBD2_FLAG_LAST_TAG,
        }
        .to_disk_bytes(&mut descriptor[12..20]);
        descriptor[20..20 + JBD2_UUID_SIZE].copy_from_slice(&journal_sb.s_uuid);
    }
    seal_jbd2_control_block(&journal_sb, &mut descriptor);

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
    let journal_sb = JournalSuperBlock::from_disk_bytes(&bytes);

    let metadata_blocks: Vec<Vec<u8>> = target_blocks
        .iter()
        .enumerate()
        .map(|(idx, _)| {
            let mut metadata = vec![0u8; BLOCK_SIZE];
            metadata[..8].copy_from_slice(&(idx as u64).to_le_bytes());
            metadata
        })
        .collect();

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
        if journal_sb.s_feature_incompat & JBD2_FEATURE_INCOMPAT_CSUM_V3 != 0 {
            JournalBlockTag3S {
                t_blocknr: *target as u32,
                t_flags: u32::from(flags),
                t_blocknr_high: (*target >> 32) as u32,
                t_checksum: jbd2_tag_checksum(&journal_sb, &metadata_blocks[idx]),
            }
            .to_disk_bytes(&mut descriptor[offset..offset + 16]);
            offset += 16;
        } else {
            JournalBlockTagS {
                t_blocknr: *target as u32,
                t_checksum: 0,
                t_flags: flags,
            }
            .to_disk_bytes(&mut descriptor[offset..offset + 8]);
            offset += 8;
        }
        if idx == 0 {
            descriptor[offset..offset + JBD2_UUID_SIZE].copy_from_slice(&journal_sb.s_uuid);
            offset += JBD2_UUID_SIZE;
        }
    }
    seal_jbd2_control_block(&journal_sb, &mut descriptor);
    device.write_block_bytes(journal_block + 1, &descriptor);

    for (idx, metadata) in metadata_blocks.iter().enumerate() {
        device.write_block_bytes(journal_block + 2 + idx as u64, metadata);
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
    let mut fs =
        Ext4FileSystem::mount(&mut remount_dev).expect("mount after intact checksum data failed");
    let read_back = read_file(&mut remount_dev, &mut fs, "/crc.txt").expect("read_file failed");
    assert_eq!(read_back, payload);
    umount(fs, &mut remount_dev).expect("umount failed");
}

#[test]
fn unclean_remount_reaps_the_persisted_classic_orphan_chain() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut first_dev = new_jbd2_dev(device.clone());
    mkfs(&mut first_dev).expect("mkfs failed");
    let mut first_fs = Ext4FileSystem::mount(&mut first_dev).expect("mount failed");

    mkfile(&mut first_dev, &mut first_fs, "/orphan-a", Some(b"a"), None)
        .expect("first file create failed");
    mkfile(&mut first_dev, &mut first_fs, "/orphan-b", Some(b"b"), None)
        .expect("second file create failed");
    let first_inode = rsext4::dir::get_inode_with_num(&mut first_fs, &mut first_dev, "/orphan-a")
        .expect("first lookup failed")
        .expect("first file missing")
        .0;
    let second_inode = rsext4::dir::get_inode_with_num(&mut first_fs, &mut first_dev, "/orphan-b")
        .expect("second lookup failed")
        .expect("second file missing")
        .0;

    let first_outcome =
        unlink(&mut first_fs, &mut first_dev, "/orphan-a").expect("first unlink failed");
    let second_outcome =
        unlink(&mut first_fs, &mut first_dev, "/orphan-b").expect("second unlink failed");
    assert!(first_outcome.requires_reap());
    assert!(second_outcome.requires_reap());
    assert_eq!(first_fs.superblock.s_last_orphan, second_inode.raw());

    // Persist the dirty transaction but deliberately skip ext4 unmount. The
    // next mount must replay JBD2 first and then drain both orphan entries.
    first_fs
        .sync_filesystem(&mut first_dev)
        .expect("dirty sync failed");
    first_dev
        .umount_commit()
        .expect("dirty journal commit failed");
    drop(first_fs);
    drop(first_dev);

    let mut remount_dev = new_jbd2_dev(device);
    let mut recovered =
        Ext4FileSystem::mount(&mut remount_dev).expect("orphan recovery mount failed");
    assert_eq!(recovered.superblock.s_last_orphan, 0);
    assert!(
        !recovered
            .inode_num_already_allocated(&mut remount_dev, first_inode)
            .expect("first orphan allocation lookup failed")
    );
    assert!(
        !recovered
            .inode_num_already_allocated(&mut remount_dev, second_inode)
            .expect("second orphan allocation lookup failed")
    );
    assert!(
        rsext4::dir::get_inode_with_num(&mut recovered, &mut remount_dev, "/orphan-a")
            .expect("post-recovery first lookup failed")
            .is_none()
    );
    assert!(
        rsext4::dir::get_inode_with_num(&mut recovered, &mut remount_dev, "/orphan-b")
            .expect("post-recovery second lookup failed")
            .is_none()
    );
    umount(recovered, &mut remount_dev).expect("recovered unmount failed");
}

#[test]
fn unclean_remount_finishes_linked_extent_truncate_from_classic_orphan() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut first_dev = new_jbd2_dev(device.clone());
    mkfs(&mut first_dev).expect("mkfs failed");
    let mut first_fs = Ext4FileSystem::mount(&mut first_dev).expect("mount failed");
    let block_size = first_fs.superblock.block_size() as usize;
    let payload = vec![0x5a; block_size * 3];
    mkfile(
        &mut first_dev,
        &mut first_fs,
        "/linked",
        Some(&payload),
        None,
    )
    .expect("file create failed");
    let inode_num = rsext4::dir::get_inode_with_num(&mut first_fs, &mut first_dev, "/linked")
        .expect("lookup failed")
        .expect("linked file missing")
        .0;

    first_fs
        .modify_inode(&mut first_dev, inode_num, |inode| {
            inode.i_size_lo = block_size as u32;
            inode.i_size_high = 0;
            inode.i_dtime = 0;
        })
        .expect("commit shortened inode size");
    first_fs.superblock.s_last_orphan = inode_num.raw();
    // This test deliberately constructs an interrupted on-disk transition by
    // editing the inspection-visible superblock copy. Publish that explicit
    // edit before the ordinary dirty-cache sync.
    first_fs
        .sync_superblock(&mut first_dev)
        .expect("persist classic orphan head");
    first_fs
        .sync_filesystem(&mut first_dev)
        .expect("dirty sync failed");
    first_dev
        .umount_commit()
        .expect("dirty journal commit failed");
    drop(first_fs);
    let device = first_dev.into_inner();

    let mut remount_dev = new_jbd2_dev(device);
    let mut recovered =
        Ext4FileSystem::mount(&mut remount_dev).expect("linked truncate recovery mount failed");
    assert_eq!(recovered.superblock.s_last_orphan, 0);
    let mut inode = recovered
        .get_inode_by_num(&mut remount_dev, inode_num)
        .expect("recovered linked inode missing");
    assert_eq!(inode.i_links_count, 1);
    assert_eq!(inode.i_dtime, 0);
    assert_eq!(inode.size(), block_size as u64);
    let mappings = resolve_inode_blocks(&mut recovered, &mut remount_dev, inode_num, &mut inode)
        .expect("resolve recovered mappings");
    assert_eq!(mappings.len(), 1);
    assert_eq!(
        mappings.first_key_value().map(|(&logical, _)| logical),
        Some(0)
    );
    let read_back =
        read_file(&mut remount_dev, &mut recovered, "/linked").expect("read recovered linked file");
    assert_eq!(read_back, payload[..block_size]);
    umount(recovered, &mut remount_dev).expect("recovered unmount failed");
}

#[test]
fn cyclic_classic_orphan_chain_is_rejected_before_recovery() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut first_dev = new_jbd2_dev(device.clone());
    mkfs(&mut first_dev).expect("mkfs failed");
    let mut first_fs = Ext4FileSystem::mount(&mut first_dev).expect("mount failed");
    mkfile(
        &mut first_dev,
        &mut first_fs,
        "/cycle",
        Some(b"owned"),
        None,
    )
    .expect("file create failed");
    let outcome = unlink(&mut first_fs, &mut first_dev, "/cycle").expect("unlink failed");
    assert!(outcome.requires_reap());

    first_fs
        .modify_inode(&mut first_dev, outcome.inode, |inode| {
            inode.i_dtime = outcome.inode.raw();
        })
        .expect("cycle injection failed");
    let reap_error = reap_unlinked_inode(&mut first_fs, &mut first_dev, outcome.inode)
        .expect_err("reap must validate the complete chain before mutation");
    assert_eq!(reap_error.kind(), Ext4ErrorKind::Corrupted);
    assert!(
        first_fs
            .inode_num_already_allocated(&mut first_dev, outcome.inode)
            .expect("cyclic orphan allocation lookup failed")
    );
    first_fs
        .sync_filesystem(&mut first_dev)
        .expect("dirty sync failed");
    first_dev
        .umount_commit()
        .expect("dirty journal commit failed");
    drop(first_fs);
    drop(first_dev);

    let mut remount_dev = new_jbd2_dev(device);
    let error = match Ext4FileSystem::mount(&mut remount_dev) {
        Ok(_) => panic!("cyclic orphan chain must not mount"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
    assert_eq!(
        error.context(),
        Some(ErrorContext::Operation { op: "orphan:cycle" })
    );
}

#[test]
fn mkfs_maps_ext4_metadata_checksum_and_64bit_features_to_jbd2() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut dev = new_jbd2_dev(device.clone());
    mkfs(&mut dev).expect("mkfs failed");
    let fs = Ext4FileSystem::mount(&mut dev).expect("mount failed");
    let journal_block = fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    let journal = JournalSuperBlock::from_disk_bytes(&device.read_block_bytes(journal_block));

    assert_ne!(
        journal.s_feature_incompat & JBD2_FEATURE_INCOMPAT_CSUM_V3,
        0
    );
    assert_ne!(journal.s_feature_incompat & JBD2_FEATURE_INCOMPAT_64BIT, 0);
    assert_eq!(journal.s_checksum_type, JBD2_CRC32C_CHKSUM);
    umount(fs, &mut dev).expect("umount failed");
}

#[test]
fn mount_accepts_v1_journal_without_reading_v2_extension_fields() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut first_dev = new_jbd2_dev(device.clone());
    mkfs(&mut first_dev).expect("mkfs failed");
    let first_fs = Ext4FileSystem::mount(&mut first_dev).expect("initial mount failed");
    let journal_block = first_fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    umount(first_fs, &mut first_dev).expect("initial unmount failed");

    let mut bytes = device.read_block_bytes(journal_block);
    let mut journal = JournalSuperBlock::decode_checked(&bytes).unwrap();
    journal.s_header.h_blocktype = JBD2_BLOCKTYPE_SUPERBLOCK_V1;
    journal.s_feature_compat = u32::MAX;
    journal.s_feature_incompat = u32::MAX;
    journal.s_feature_ro_compat = u32::MAX;
    journal.s_uuid = [0xff; 16];
    journal.s_checksum_type = u8::MAX;
    journal.s_checksum = 0xa5a5_5a5a;
    journal.to_disk_bytes(&mut bytes);
    device.write_block_bytes(journal_block, &bytes);

    let mut remount_dev = new_jbd2_dev(device);
    let remounted =
        Ext4FileSystem::mount(&mut remount_dev).expect("Linux-compatible v1 journal mount");
    umount(remounted, &mut remount_dev).expect("v1 journal unmount");
}

#[test]
fn mount_accepts_internal_journal_uuid_distinct_from_filesystem_uuid() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut first_dev = new_jbd2_dev(device.clone());
    mkfs(&mut first_dev).expect("mkfs failed");
    let first_fs = Ext4FileSystem::mount(&mut first_dev).expect("initial mount failed");
    let filesystem_uuid = first_fs.superblock.s_uuid;
    let journal_block = first_fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    umount(first_fs, &mut first_dev).expect("initial unmount failed");

    let mut bytes = device.read_block_bytes(journal_block);
    let mut journal = JournalSuperBlock::decode_checked(&bytes).unwrap();
    journal.s_uuid = filesystem_uuid.map(|byte| !byte);
    jbd2_update_superblock_checksum(&mut journal);
    journal.to_disk_bytes(&mut bytes);
    device.write_block_bytes(journal_block, &bytes);

    let mut remount_dev = new_jbd2_dev(device);
    let remounted = Ext4FileSystem::mount(&mut remount_dev)
        .expect("Linux accepts an independent UUID for an internal journal");
    umount(remounted, &mut remount_dev).expect("internal journal unmount");
}

#[test]
fn axfs_ng_sync_order_preserves_inode_bitmap_across_remount() {
    // Test idea: mirror axfs-ng's sync_to_disk ordering, then remount and keep
    // creating files. Inodes allocated before the sync must remain marked in
    // the persisted inode bitmap and must not be reused after remount.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut first_dev = new_jbd2_dev(device.clone());
    mkfs(&mut first_dev).expect("mkfs failed");
    let mut fs = Ext4FileSystem::mount(&mut first_dev).expect("mount failed");

    let mut seen = BTreeSet::new();
    for idx in 0..256 {
        let path = format!("/before-{idx}");
        mkfile(&mut first_dev, &mut fs, &path, Some(b"x"), None).expect("mkfile before failed");
        let file = get_file_inode(&mut fs, &mut first_dev, &path)
            .expect("lookup before failed")
            .expect("file before missing");
        assert!(seen.insert(file.0.raw()), "duplicate inode before sync");
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
    let mut fs =
        Ext4FileSystem::mount(&mut remount_dev).expect("mount after axfs-ng order sync failed");

    for idx in 0..256 {
        let path = format!("/after-{idx}");
        mkfile(&mut remount_dev, &mut fs, &path, Some(b"y"), None).expect("mkfile after failed");
        let file = get_file_inode(&mut fs, &mut remount_dev, &path)
            .expect("lookup after failed")
            .expect("file after missing");
        assert!(
            seen.insert(file.0.raw()),
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
    let fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
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
    let fs = Ext4FileSystem::mount(&mut remount_dev)
        .expect("clean mount should not force journal replay");
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
    let fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
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
    let fs = Ext4FileSystem::mount(&mut remount_dev)
        .expect("mount should discard uncommitted journal tail");
    assert_eq!(
        fs.superblock.s_feature_incompat & Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER,
        0
    );
    umount(fs, &mut remount_dev).expect("umount failed");

    assert_eq!(device.read_block_bytes(target_block), original_target);

    let recovered_journal = device.read_block_bytes(journal_block);
    let recovered_journal_sb = JournalSuperBlock::from_disk_bytes(&recovered_journal);
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
    let fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
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
    let fs =
        Ext4FileSystem::mount(&mut remount_dev).expect("uncommitted payload should not be read");
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
    let fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
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
    let err = match Ext4FileSystem::mount(&mut remount_dev) {
        Ok(_) => panic!("invalid revoke block should fail recovery"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), Ext4ErrorKind::Corrupted);
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
    let fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
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
    let err = match Ext4FileSystem::mount(&mut writable_dev) {
        Ok(_) => panic!("default mount should fail unrecoverable journal replay"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), Ext4ErrorKind::Corrupted);

    let mut readonly_dev = Jbd2Dev::initial_jbd2dev(0, device.clone(), false);
    let fs = Ext4FileSystem::mount_with_options(
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
fn owned_readonly_fallback_preserves_unrecoverable_journal_error() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut format_dev = new_jbd2_dev(device.clone());
    mkfs(&mut format_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
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

    let services = MountServices::new(OwnedTestClock, (), NoopObserver);
    let error = match Ext4::mount_with_readonly_fallback(device, services) {
        Ok(_) => panic!("fallback must not hide an unrecoverable journal"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
}

#[test]
fn empty_descriptor_header_is_discarded_during_recovery() {
    // Test idea: a crash can leave only the descriptor header without any tags.
    // With no commit block, this is an uncommitted tail rather than durable work.
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
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
    let fs = Ext4FileSystem::mount(&mut remount_dev)
        .expect("mount should discard empty descriptor tail");
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
    let fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
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
    let err = match Ext4FileSystem::mount(&mut remount_dev) {
        Ok(_) => panic!("cyclic journal scan should fail recovery"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), Ext4ErrorKind::Corrupted);
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
    let fs =
        Ext4FileSystem::mount(&mut remount_dev).expect("mount should resolve existing lost+found");
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
    let mut fs = Ext4FileSystem::mount(&mut inspect_dev).expect("mount failed");
    let mut root = fs.get_root(&mut inspect_dev).expect("root inode");
    let root_ino = fs.root_inode;
    let root_block = resolve_inode_block(&fs, &mut inspect_dev, root_ino, &mut root, 0)
        .expect("resolve root block")
        .expect("root directory block")
        .raw();
    umount(fs, &mut inspect_dev).expect("umount failed");

    let clean_sb = read_superblock(&device);
    assert_ne!(clean_sb.s_lpf_ino, 0);

    device.blocked_read_block.set(Some(root_block));
    let mut remount_dev = new_jbd2_dev(device.clone());
    let fs =
        Ext4FileSystem::mount(&mut remount_dev).expect("mount should trust valid lost+found hint");
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
        let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
        fs.sync_superblock(&mut jbd2_dev)
            .expect("persist dirty mount state");
    }

    let dirty_sb = read_superblock(&device);
    assert_eq!(dirty_sb.s_state & Ext4Superblock::EXT4_VALID_FS, 0);
    assert_eq!(dirty_sb.s_state & Ext4Superblock::EXT4_ERROR_FS, 0);

    let mut remount_dev = new_jbd2_dev(device.clone());
    let fs = Ext4FileSystem::mount(&mut remount_dev).expect("mount after unclean shutdown failed");
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
    let fs = Ext4FileSystem::mount(&mut remount_dev).expect("mount with error state failed");
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
    let fs = Ext4FileSystem::mount(&mut remount_dev)
        .expect("mount should replay needs_recovery journal");
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
    let err = match Ext4FileSystem::mount(&mut remount_dev) {
        Ok(_) => panic!("mount should fail on corrupted superblock CRC"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), Ext4ErrorKind::ChecksumMismatch);
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
    let err = match Ext4FileSystem::mount(&mut remount_dev) {
        Ok(_) => panic!("mount should fail on corrupted GDT CRC"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), Ext4ErrorKind::ChecksumMismatch);
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
    let err = match Ext4FileSystem::mount(&mut remount_dev) {
        Ok(_) => panic!("mount should fail on corrupted bitmap payload"),
        Err(err) => err,
    };
    assert_eq!(err.kind(), Ext4ErrorKind::ChecksumMismatch);
}

#[test]
fn mount_returns_journal_superblock_read_failure_without_panicking() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
    let journal_block = fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();
    umount(fs, &mut first_mount_dev).expect("umount failed");

    device.blocked_read_block.set(Some(journal_block));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut remount_dev = new_jbd2_dev(device.clone());
        Ext4FileSystem::mount(&mut remount_dev)
    }));

    assert!(
        result.is_ok(),
        "journal superblock I/O failure must not panic"
    );
    let Err(error) = result.unwrap() else {
        panic!("mount must fail");
    };
    assert_eq!(error.kind(), Ext4ErrorKind::Io);
}

#[test]
fn mount_returns_bitmap_read_failures_without_panicking() {
    let (device, _) = build_filesystem_with_written_file();
    let superblock = read_superblock(&device);
    let group_desc = read_group_desc0(&device, &superblock);

    for bitmap_block in [group_desc.inode_bitmap(), group_desc.block_bitmap()] {
        device.blocked_read_block.set(Some(bitmap_block));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut remount_dev = new_jbd2_dev(device.clone());
            Ext4FileSystem::mount(&mut remount_dev)
        }));

        assert!(result.is_ok(), "bitmap I/O failure must not panic");
        let Err(error) = result.unwrap() else {
            panic!("mount must fail");
        };
        assert_eq!(error.kind(), Ext4ErrorKind::Io);
    }
}

#[test]
fn mount_rejects_an_empty_journal_mapping_without_panicking() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let mut fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
    let journal_inode = InodeNumber::new(JOURNAL_FILE_INODE as u32).expect("valid journal inode");
    fs.modify_inode(&mut first_mount_dev, journal_inode, |inode| {
        inode.i_size_lo = 0;
        inode.i_size_high = 0;
    })
    .expect("corrupt journal inode mapping");
    sync_with_axfs_ng_order(&mut first_mount_dev, &mut fs).expect("persist corrupted mapping");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut remount_dev = new_jbd2_dev(device.clone());
        Ext4FileSystem::mount(&mut remount_dev)
    }));

    assert!(result.is_ok(), "invalid journal mapping must not panic");
    let Err(error) = result.unwrap() else {
        panic!("mount must reject an empty journal mapping");
    };
    assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
}

#[test]
fn mount_rejects_missing_journal_inode_without_panicking() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let mut fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
    let journal_inode = InodeNumber::new(JOURNAL_FILE_INODE as u32).expect("valid journal inode");
    fs.modify_inode(&mut first_mount_dev, journal_inode, |inode| {
        inode.i_mode = 0
    })
    .expect("remove journal inode");
    sync_with_axfs_ng_order(&mut first_mount_dev, &mut fs).expect("persist missing journal inode");

    let mut superblock = read_superblock(&device);
    superblock.s_feature_incompat &= !Ext4Superblock::EXT4_FEATURE_INCOMPAT_RECOVER;
    superblock.update_checksum();
    write_superblock(&device, &superblock);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut remount_dev = new_jbd2_dev(device.clone());
        Ext4FileSystem::mount(&mut remount_dev)
    }));

    assert!(result.is_ok(), "a missing journal inode must not panic");
    let Err(error) = result.unwrap() else {
        panic!("mount must reject a missing journal inode");
    };
    assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
}

#[test]
fn mount_rejects_encrypted_journal_inode_without_panicking() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut first_mount_dev = new_jbd2_dev(device.clone());
    let mut fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
    let journal_inode = InodeNumber::new(JOURNAL_FILE_INODE as u32).expect("valid journal inode");
    fs.modify_inode(&mut first_mount_dev, journal_inode, |inode| {
        inode.i_flags |= Ext4Inode::EXT4_ENCRYPT_FL;
    })
    .expect("encrypt journal inode");
    sync_with_axfs_ng_order(&mut first_mount_dev, &mut fs)
        .expect("persist encrypted journal inode");

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut remount_dev = new_jbd2_dev(device.clone());
        Ext4FileSystem::mount(&mut remount_dev)
    }));

    assert!(result.is_ok(), "an encrypted journal inode must not panic");
    let Err(error) = result.unwrap() else {
        panic!("mount must reject an encrypted journal inode");
    };
    assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
    assert_eq!(
        error.context(),
        Some(ErrorContext::Operation {
            op: "journal:invalid_inode"
        })
    );
}

#[test]
fn mount_rejects_unlinked_and_non_regular_journal_inodes() {
    for invalid_kind in ["unlinked", "directory"] {
        let device = SharedCrcDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = new_jbd2_dev(device.clone());
        mkfs(&mut jbd2_dev).expect("mkfs failed");

        let mut first_mount_dev = new_jbd2_dev(device.clone());
        let mut fs = Ext4FileSystem::mount(&mut first_mount_dev).expect("mount failed");
        let journal_inode =
            InodeNumber::new(JOURNAL_FILE_INODE as u32).expect("valid journal inode");
        fs.modify_inode(
            &mut first_mount_dev,
            journal_inode,
            |inode| match invalid_kind {
                "unlinked" => inode.i_links_count = 0,
                "directory" => inode.i_mode = Ext4Inode::S_IFDIR | 0o700,
                _ => unreachable!("fixed invalid journal fixture"),
            },
        )
        .expect("invalidate journal inode");
        sync_with_axfs_ng_order(&mut first_mount_dev, &mut fs)
            .expect("persist invalid journal inode");

        let mut remount_dev = new_jbd2_dev(device);
        let error = match Ext4FileSystem::mount(&mut remount_dev) {
            Ok(_) => panic!("mount must reject a {invalid_kind} journal inode"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), Ext4ErrorKind::Corrupted);
        assert_eq!(
            error.context(),
            Some(ErrorContext::Operation {
                op: "journal:invalid_inode"
            })
        );
    }
}

#[test]
fn mount_rejects_both_internal_and_external_journal_declarations() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = new_jbd2_dev(device.clone());
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut superblock = read_superblock(&device);
    assert_ne!(superblock.s_journal_inum, 0);
    superblock.s_journal_dev = 1;
    superblock.update_checksum();
    write_superblock(&device, &superblock);
    let image_before = device.data.borrow().clone();

    let mut mount_dev = new_jbd2_dev(device.clone());
    let error = match Ext4FileSystem::mount(&mut mount_dev) {
        Ok(_) => panic!("mount must reject simultaneous journal inode and device"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), Ext4ErrorKind::InvalidInput);
    assert_eq!(
        error.context(),
        Some(ErrorContext::Operation {
            op: "journal:ambiguous_source"
        })
    );
    assert_eq!(*device.data.borrow(), image_before);
}

#[test]
fn mount_reports_external_or_missing_journal_source_without_mutation() {
    for (journal_inode, journal_device, expected_kind, expected_context) in [
        (
            0,
            1,
            Ext4ErrorKind::UnsupportedCapability,
            ErrorContext::Capability {
                name: "block_io:external_journal",
            },
        ),
        (
            0,
            0,
            Ext4ErrorKind::Corrupted,
            ErrorContext::Operation {
                op: "journal:missing_source",
            },
        ),
    ] {
        let device = SharedCrcDevice::new(100 * 1024 * 1024);
        let mut jbd2_dev = new_jbd2_dev(device.clone());
        mkfs(&mut jbd2_dev).expect("mkfs failed");

        let mut superblock = read_superblock(&device);
        superblock.s_journal_inum = journal_inode;
        superblock.s_journal_dev = journal_device;
        superblock.update_checksum();
        write_superblock(&device, &superblock);
        let image_before = device.data.borrow().clone();

        let mut mount_dev = new_jbd2_dev(device.clone());
        let error = match Ext4FileSystem::mount(&mut mount_dev) {
            Ok(_) => panic!("mount must reject an unavailable journal source"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), expected_kind);
        assert_eq!(error.context(), Some(expected_context));
        assert_eq!(*device.data.borrow(), image_before);
    }
}

#[test]
fn journal_start_write_failure_rolls_back_and_aborts_without_retry() {
    let device = SharedCrcDevice::new(100 * 1024 * 1024);
    let mut format_dev = new_jbd2_dev(device.clone());
    mkfs(&mut format_dev).expect("mkfs failed");

    let mut dev = new_jbd2_dev(device.clone());
    let fs = Ext4FileSystem::mount(&mut dev).expect("mount failed");
    let journal_block = fs
        .journal_sb_block_start
        .expect("journal superblock should be mapped")
        .raw();

    let unchanged_home_block = AbsoluteBN::new(42);
    dev.read_block(unchanged_home_block)
        .expect("read home block");
    let unchanged_home_image = dev.buffer().to_vec();
    dev.write_blocks(&unchanged_home_image, unchanged_home_block, 1, true)
        .expect("queue metadata update");

    device.failing_write_block.set(Some(journal_block));
    device.failing_write_attempts.borrow_mut().insert(1);

    let first_error = dev
        .umount_commit()
        .expect_err("initial journal superblock write must fail");
    assert_eq!(first_error.kind(), Ext4ErrorKind::Io);

    let abort_error = dev
        .umount_commit()
        .expect_err("an aborted journal must reject transaction retry");
    assert_eq!(abort_error.kind(), Ext4ErrorKind::JournalAborted);

    let on_disk_journal =
        JournalSuperBlock::from_disk_bytes(&device.read_block_bytes(journal_block));
    assert_eq!(
        on_disk_journal.s_start, 0,
        "a failed replay-start write must not publish a partial transaction"
    );
}
