use alloc::vec;

use super::*;
use crate::{
    config::BLOCK_SIZE,
    endian::DiskFormat,
    jbd2::jbdstruct::{
        CommitHeader, JBD2_BLOCKTYPE_REVOKE, JBD2_COMMIT_HEADER_SIZE, JBD2_CRC32C_CHKSUM,
        JBD2_DESCRIPTOR_HEADER_SIZE, JBD2_TAG3_SIZE, JBD2_UUID_SIZE, JOURNAL_ESCAPE,
        Jbd2CommitPhase, Jbd2JournalRevokeHeadS, JournalBlockTag3S, JournalBlockTagS,
        JournalHeaderS,
    },
};

struct MemBlockDev {
    data: Vec<u8>,
    fail_flush: bool,
    fail_fua: bool,
    fail_read_sector: Option<u64>,
    fail_write_sector: Option<u64>,
    fail_write_call: Option<usize>,
    fail_flush_call: Option<usize>,
    write_calls: usize,
    flush_calls: usize,
    fua_writes: usize,
}

impl MemBlockDev {
    fn new(blocks: usize) -> Self {
        Self {
            data: vec![0; blocks * BLOCK_SIZE],
            fail_flush: false,
            fail_fua: false,
            fail_read_sector: None,
            fail_write_sector: None,
            fail_write_call: None,
            fail_flush_call: None,
            write_calls: 0,
            flush_calls: 0,
            fua_writes: 0,
        }
    }

    fn with_failing_flush(blocks: usize) -> Self {
        Self {
            data: vec![0; blocks * BLOCK_SIZE],
            fail_flush: true,
            fail_fua: false,
            fail_read_sector: None,
            fail_write_sector: None,
            fail_write_call: None,
            fail_flush_call: None,
            write_calls: 0,
            flush_calls: 0,
            fua_writes: 0,
        }
    }

    fn with_failing_flush_and_fua(blocks: usize) -> Self {
        Self {
            data: vec![0; blocks * BLOCK_SIZE],
            fail_flush: true,
            fail_fua: true,
            fail_read_sector: None,
            fail_write_sector: None,
            fail_write_call: None,
            fail_flush_call: None,
            write_calls: 0,
            flush_calls: 0,
            fua_writes: 0,
        }
    }

    fn sector_for_filesystem_block(&self, block: AbsoluteBN) -> u64 {
        let sector_size = self.geometry().logical_block_size as usize;
        assert!(BLOCK_SIZE.is_multiple_of(sector_size));
        block
            .raw()
            .checked_mul((BLOCK_SIZE / sector_size) as u64)
            .expect("test filesystem block must map to a device sector")
    }

    fn fail_next_read_at_block(&mut self, block: AbsoluteBN) {
        self.fail_read_sector = Some(self.sector_for_filesystem_block(block));
    }

    fn fail_next_write_at_block(&mut self, block: AbsoluteBN) {
        self.fail_write_sector = Some(self.sector_for_filesystem_block(block));
    }

    fn with_failing_write_call(blocks: usize, call: usize) -> Self {
        let mut device = Self::new(blocks);
        device.fail_write_call = Some(call);
        device
    }

    fn with_failing_flush_call(blocks: usize, call: usize) -> Self {
        let mut device = Self::new(blocks);
        device.fail_flush_call = Some(call);
        device
    }
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

fn reference_crc32_be(mut crc: u32, bytes: &[u8]) -> u32 {
    for &byte in bytes {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn reference_jbd2_seed(uuid: &[u8; JBD2_UUID_SIZE]) -> u32 {
    reference_crc32c(u32::MAX, uuid)
}

fn reference_jbd2_tag_checksum(uuid: &[u8; JBD2_UUID_SIZE], sequence: u32, payload: &[u8]) -> u32 {
    let checksum = reference_crc32c(reference_jbd2_seed(uuid), &sequence.to_be_bytes());
    reference_crc32c(checksum, payload)
}

fn reference_jbd2_block_checksum(
    uuid: &[u8; JBD2_UUID_SIZE],
    block: &[u8],
    checksum_offset: usize,
) -> u32 {
    let checksum = reference_crc32c(reference_jbd2_seed(uuid), &block[..checksum_offset]);
    let checksum = reference_crc32c(checksum, &[0; 4]);
    reference_crc32c(checksum, &block[checksum_offset + 4..])
}

fn csum_v3_superblock() -> JournalSuperBlock {
    let mut superblock = JournalSuperBlock {
        s_maxlen: 64,
        s_feature_incompat: JBD2_FEATURE_INCOMPAT_64BIT | JBD2_FEATURE_INCOMPAT_CSUM_V3,
        s_checksum_type: JBD2_CRC32C_CHKSUM,
        s_uuid: [0x5a; JBD2_UUID_SIZE],
        ..Default::default()
    };
    crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
    superblock
}

fn csum_v2_superblock() -> JournalSuperBlock {
    let mut superblock = JournalSuperBlock {
        s_maxlen: 64,
        s_feature_incompat: JBD2_FEATURE_INCOMPAT_CSUM_V2,
        s_checksum_type: JBD2_CRC32C_CHKSUM,
        s_uuid: [0x3c; JBD2_UUID_SIZE],
        ..Default::default()
    };
    crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
    superblock
}

fn compat_checksum_superblock() -> JournalSuperBlock {
    JournalSuperBlock {
        s_maxlen: 64,
        s_feature_compat: JBD2_FEATURE_COMPAT_CHECKSUM,
        s_uuid: [0x27; JBD2_UUID_SIZE],
        ..JournalSuperBlock::default()
    }
}

fn small_journal_superblock() -> JournalSuperBlock {
    JournalSuperBlock {
        s_maxlen: 16,
        s_first: 1,
        ..JournalSuperBlock::default()
    }
}

fn committed_csum_v3_fixture() -> (MemBlockDev, JournalSuperBlock, AbsoluteBN) {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    let superblock = csum_v3_superblock();
    dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
        .expect("install csum-v3 journal");

    let target = AbsoluteBN::new(10);
    let payload = vec![0xa5; BLOCK_SIZE];
    dev.write_blocks(&payload, target, 1, true)
        .expect("queue csum-v3 metadata");
    dev.umount_commit().expect("commit csum-v3 metadata");

    let mut inner = dev.into_inner();
    inner.write_calls = 0;
    inner.flush_calls = 0;
    inner.fua_writes = 0;
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    inner.data[target_start..target_start + BLOCK_SIZE].fill(0);

    let mut replay_superblock = superblock;
    replay_superblock.s_start = replay_superblock.s_first;
    replay_superblock.s_sequence = 1;
    crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
    replay_superblock.to_disk_bytes(&mut inner.data[128 * BLOCK_SIZE..][..1024]);

    (inner, replay_superblock, target)
}

fn committed_csum_v2_fixture_with_features(
    extra_incompat: u32,
) -> (MemBlockDev, JournalSuperBlock, AbsoluteBN) {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    let mut superblock = csum_v2_superblock();
    superblock.s_feature_incompat |= extra_incompat;
    crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
    dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
        .expect("install csum-v2 journal");

    let target = AbsoluteBN::new(10);
    let payload = vec![0x6d; BLOCK_SIZE];
    dev.write_blocks(&payload, target, 1, true)
        .expect("queue csum-v2 metadata");
    dev.umount_commit().expect("commit csum-v2 metadata");

    let mut inner = dev.into_inner();
    inner.write_calls = 0;
    inner.flush_calls = 0;
    inner.fua_writes = 0;
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    inner.data[target_start..target_start + BLOCK_SIZE].fill(0);

    let mut replay_superblock = superblock;
    replay_superblock.s_start = replay_superblock.s_first;
    replay_superblock.s_sequence = 1;
    crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
    replay_superblock.to_disk_bytes(&mut inner.data[128 * BLOCK_SIZE..][..1024]);

    (inner, replay_superblock, target)
}

fn committed_csum_v2_fixture() -> (MemBlockDev, JournalSuperBlock, AbsoluteBN) {
    committed_csum_v2_fixture_with_features(0)
}

fn committed_compat_checksum_fixture() -> (MemBlockDev, JournalSuperBlock, AbsoluteBN) {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
    let superblock = compat_checksum_superblock();
    dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
        .expect("install compat-checksum journal");

    let target = AbsoluteBN::new(10);
    let payload = vec![0x93; BLOCK_SIZE];
    dev.write_blocks(&payload, target, 1, true)
        .expect("queue compat-checksum metadata");
    dev.umount_commit()
        .expect("commit compat-checksum metadata");

    let mut inner = dev.into_inner();
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    inner.data[target_start..target_start + BLOCK_SIZE].fill(0);
    let mut replay_superblock = superblock;
    replay_superblock.s_start = replay_superblock.s_first;
    replay_superblock.s_sequence = 1;
    replay_superblock.to_disk_bytes(&mut inner.data[128 * BLOCK_SIZE..][..1024]);
    (inner, replay_superblock, target)
}

fn replay_csum_v3_fixture(
    inner: MemBlockDev,
    superblock: JournalSuperBlock,
    target: AbsoluteBN,
) -> (ReplayStatus, MemBlockDev) {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    let journal_blocks = (128..192).map(AbsoluteBN::new).collect();
    dev.set_journal_superblock_with_mapping(superblock, journal_blocks)
        .expect("install csum-v3 journal");
    let status = dev.journal_replay_checked();
    if status.failure().is_some() {
        let error = dev
            .set_journal_use(false)
            .expect_err("incomplete replay must latch the journal abort");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);
    }
    let inner = dev.into_inner();
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    assert_eq!(
        &inner.data[target_start..target_start + BLOCK_SIZE],
        vec![0; BLOCK_SIZE]
    );
    (status, inner)
}

fn replay_csum_v2_fixture(
    inner: MemBlockDev,
    superblock: JournalSuperBlock,
    target: AbsoluteBN,
) -> (ReplayStatus, MemBlockDev) {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    let journal_blocks = (128..192).map(AbsoluteBN::new).collect();
    dev.set_journal_superblock_with_mapping(superblock, journal_blocks)
        .expect("install csum-v2 journal");
    let status = dev.journal_replay_checked();
    if status.failure().is_some() {
        let error = dev
            .set_journal_use(false)
            .expect_err("incomplete replay must latch the journal abort");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);
    }
    let inner = dev.into_inner();
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    if status.failure().is_some() {
        assert_eq!(
            &inner.data[target_start..target_start + BLOCK_SIZE],
            vec![0; BLOCK_SIZE]
        );
    }
    (status, inner)
}

fn replay_compat_checksum_fixture(
    inner: MemBlockDev,
    superblock: JournalSuperBlock,
    target: AbsoluteBN,
) -> (ReplayStatus, MemBlockDev) {
    let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
    let journal_blocks = (128..192).map(AbsoluteBN::new).collect();
    dev.set_journal_superblock_with_mapping(superblock, journal_blocks)
        .expect("install compat-checksum journal");
    let status = dev.journal_replay_checked();
    if status.failure().is_some() {
        let error = dev
            .set_journal_use(false)
            .expect_err("incomplete replay must latch the journal abort");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);
    }
    let inner = dev.into_inner();
    let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
    if status.failure().is_some() {
        assert_eq!(
            &inner.data[target_start..target_start + BLOCK_SIZE],
            vec![0; BLOCK_SIZE]
        );
    }
    (status, inner)
}

impl BlockIo for MemBlockDev {
    fn read(
        &mut self,
        buffer: &mut [u8],
        block_id: crate::io::SectorId,
        _count: u32,
    ) -> Ext4Result<()> {
        if self.fail_read_sector == Some(block_id.raw()) {
            self.fail_read_sector = None;
            return Err(Ext4Error::io());
        }
        let start = block_id.as_usize()? * BLOCK_SIZE;
        let end = start + buffer.len();
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(
        &mut self,
        buffer: &[u8],
        block_id: crate::io::SectorId,
        _count: u32,
    ) -> Ext4Result<()> {
        self.write_calls += 1;
        if self.fail_write_call == Some(self.write_calls) {
            self.fail_write_call = None;
            return Err(Ext4Error::io());
        }
        if self.fail_write_sector == Some(block_id.raw()) {
            self.fail_write_sector = None;
            return Err(Ext4Error::io());
        }
        let start = block_id.as_usize()? * BLOCK_SIZE;
        let end = start + buffer.len();
        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn write_with_flags(
        &mut self,
        buffer: &[u8],
        block_id: crate::io::SectorId,
        count: u32,
        flags: crate::WriteFlags,
    ) -> Ext4Result<()> {
        if flags.contains(crate::WriteFlags::FUA) {
            self.fua_writes += 1;
            if self.fail_fua {
                return Err(Ext4Error::io());
            }
        }
        self.write(buffer, block_id, count)
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.flush_calls += 1;
        if self.fail_flush_call == Some(self.flush_calls) {
            self.fail_flush_call = None;
            return Err(Ext4Error::io());
        }
        if core::mem::take(&mut self.fail_flush) {
            Err(Ext4Error::io())
        } else {
            Ok(())
        }
    }

    fn geometry(&self) -> crate::io::DeviceGeometry {
        crate::io::DeviceGeometry::new(BLOCK_SIZE as u32, (self.data.len() / BLOCK_SIZE) as u64)
    }

    fn capabilities(&self) -> crate::io::DeviceCapabilities {
        crate::io::DeviceCapabilities {
            read_only: { false },

            flush: true,

            fua: true,

            ..crate::io::DeviceCapabilities::default()
        }
    }
}

impl crate::runtime::Clock for MemBlockDev {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::new(0, 0))
    }
}

struct FixedCommitClock;

impl crate::runtime::Clock for FixedCommitClock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::new(1_723_456_789, 123_456_789))
    }
}

struct FailingCommitClock;

impl crate::runtime::Clock for FailingCommitClock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Err(Ext4Error::io().with_operation("test:unexpected_empty_commit_clock"))
    }
}

struct InvalidCommitClock(Ext4Timestamp);

impl crate::runtime::Clock for InvalidCommitClock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(self.0)
    }
}

mod abort;
mod block_io;
mod checkpoint;
mod commit_format;
mod credits;
mod edit_rollback;
mod handles;
mod recovery;
mod revoke;
