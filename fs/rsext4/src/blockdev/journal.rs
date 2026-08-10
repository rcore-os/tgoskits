//! JBD2-aware block device facade.

use alloc::vec::Vec;

use super::{FilesystemBlockIo, cached_device::BlockDev};
use crate::{
    bmalloc::AbsoluteBN,
    checksum::jbd2_superblock_csum32,
    config::JBD2_BUFFER_MAX,
    disknode::Ext4Timestamp,
    error::{Ext4Error, Ext4Result},
    io::BlockIo,
    jbd2::{
        jbd2::ReplayStatus,
        jbdstruct::{
            JBD2_BLOCKTYPE_SUPERBLOCK_V2, JBD2_CRC32C_CHKSUM, JBD2_FEATURE_INCOMPAT_64BIT,
            JBD2_FEATURE_INCOMPAT_CSUM_V3, JBD2_MAGIC, JBD2DEVSYSTEM, Jbd2Update,
            JournalSuperBllockS,
        },
    },
    runtime::Clock,
};

/// Runtime state of the journal proxy.
pub enum Jbd2RunState {
    Commit,
    Replay,
}

/// Block device proxy that optionally routes metadata writes through JBD2.
pub struct Jbd2Dev<B: BlockIo> {
    _mode: u8,
    inner: BlockDev<B>,
    journal_use: bool,
    _state: Jbd2RunState,
    system: Option<JBD2DEVSYSTEM>,
    journal_blocks: Vec<AbsoluteBN>,
}

impl<B: BlockIo> Jbd2Dev<B> {
    fn validate_journal_superblock(
        &self,
        super_block: &JournalSuperBllockS,
        mapped_blocks: usize,
    ) -> Ext4Result<()> {
        if super_block.s_header.h_magic != JBD2_MAGIC
            || super_block.s_header.h_blocktype != JBD2_BLOCKTYPE_SUPERBLOCK_V2
        {
            return Err(Ext4Error::corrupted().with_operation("jbd2:superblock_header"));
        }
        if super_block.s_blocksize != self.inner.block_size() {
            return Err(Ext4Error::bad_superblock().with_operation("jbd2:block_size"));
        }
        let mapped_blocks = u32::try_from(mapped_blocks).map_err(|_| Ext4Error::overflow())?;
        if super_block.s_maxlen == 0
            || super_block.s_maxlen > mapped_blocks
            || super_block.s_first == 0
            || super_block.s_first >= super_block.s_maxlen
            || (super_block.s_start != 0
                && (super_block.s_start < super_block.s_first
                    || super_block.s_start >= super_block.s_maxlen))
        {
            return Err(Ext4Error::corrupted().with_operation("jbd2:ring_geometry"));
        }
        let supported_incompat = JBD2_FEATURE_INCOMPAT_64BIT | JBD2_FEATURE_INCOMPAT_CSUM_V3;
        if super_block.s_feature_incompat & !supported_incompat != 0 {
            return Err(Ext4Error::unsupported().with_operation("jbd2:features"));
        }
        match super_block.s_checksum_type {
            0 => {}
            JBD2_CRC32C_CHKSUM => {
                if super_block.s_checksum != jbd2_superblock_csum32(super_block) {
                    return Err(Ext4Error::checksum().with_operation("jbd2:superblock_checksum"));
                }
            }
            _ => return Err(Ext4Error::unsupported().with_operation("jbd2:checksum_type")),
        }
        if super_block.s_errno != 0 {
            return Err(Ext4Error::journal_aborted().with_operation("jbd2:recorded_error"));
        }
        Ok(())
    }

    fn enqueue_journal_update<D: FilesystemBlockIo>(
        system: &mut JBD2DEVSYSTEM,
        block_dev: &mut D,
        journal_blocks: &[AbsoluteBN],
        update: Jbd2Update,
    ) -> Ext4Result<bool> {
        if let Some(existing) = system
            .commit_queue
            .iter_mut()
            .find(|queued| queued.0 == update.0)
        {
            *existing = update;
            return Ok(false);
        }

        let mut committed = false;
        if system.commit_queue.len() >= JBD2_BUFFER_MAX {
            system.commit_transaction_with_mapping(block_dev, journal_blocks)?;
            committed = true;
        }

        system.commit_queue.push(update);
        Ok(committed)
    }

    fn make_system(
        super_block: JournalSuperBllockS,
        journal_start_block: AbsoluteBN,
    ) -> JBD2DEVSYSTEM {
        JBD2DEVSYSTEM {
            start_block: journal_start_block,
            max_len: super_block.s_maxlen,
            head: 0,
            sequence: super_block.s_sequence,
            jbd2_super_block: super_block,
            commit_queue: Vec::new(),
        }
    }

    /// Creates a new JBD2 block device proxy.
    pub fn initial_jbd2dev(_mode: u8, block_dev: B, use_journal: bool) -> Self {
        let block_dev = BlockDev::new(block_dev);
        Self {
            _mode,
            inner: block_dev,
            journal_use: use_journal,
            _state: Jbd2RunState::Commit,
            system: None,
            journal_blocks: Vec::new(),
        }
    }

    pub fn into_inner(self) -> B {
        self.inner.into_inner()
    }

    pub(crate) fn set_filesystem_block_size(&mut self, block_size: usize) -> Ext4Result<()> {
        self.inner.set_filesystem_block_size(block_size)
    }

    pub(crate) fn read_device_bytes(&mut self, offset: u64, output: &mut [u8]) -> Ext4Result<()> {
        self.inner.read_device_bytes(offset, output)
    }

    /// Returns whether journal support is enabled.
    pub fn is_use_journal(&self) -> bool {
        self.journal_use
    }

    /// Returns the current journal transaction sequence if journal is active.
    pub fn journal_sequence(&self) -> Option<u32> {
        self.system.as_ref().map(|s| s.sequence)
    }

    /// Replays the journal if JBD2 state is available.
    ///
    /// Returning `Incomplete` here is intentionally conservative: callers that
    /// need recovery correctness should abort rather than continue with direct
    /// writes when the filesystem advertises a journal but no journal state was
    /// installed.
    pub(crate) fn journal_replay_checked(&mut self) -> ReplayStatus {
        if !self.journal_use {
            return ReplayStatus::Complete;
        }

        let Some(jbd_sys) = self.system.as_mut() else {
            return ReplayStatus::Incomplete;
        };

        let status = jbd_sys.replay_with_mapping(&mut self.inner, &self.journal_blocks);
        if self.inner.invalidate_cache().is_err() {
            return ReplayStatus::Incomplete;
        }
        status
    }

    /// Enables or disables journal use at runtime.
    pub fn set_journal_use(&mut self, use_journal: bool) {
        self.journal_use = use_journal;
    }

    /// Installs the journal superblock so JBD2 state can be initialized lazily.
    pub fn set_journal_superblock(
        &mut self,
        super_block: JournalSuperBllockS,
        journal_start_block: AbsoluteBN,
    ) {
        self.journal_blocks.clear();
        self.system = Some(Self::make_system(super_block, journal_start_block));
    }

    pub(crate) fn set_journal_superblock_with_mapping(
        &mut self,
        super_block: JournalSuperBllockS,
        journal_blocks: Vec<AbsoluteBN>,
    ) -> Ext4Result<()> {
        let Some(&journal_start_block) = journal_blocks.first() else {
            self.journal_blocks.clear();
            self.system = None;
            return Err(Ext4Error::corrupted());
        };
        self.validate_journal_superblock(&super_block, journal_blocks.len())?;
        self.journal_blocks = journal_blocks;
        self.system = Some(Self::make_system(super_block, journal_start_block));
        Ok(())
    }

    /// Commits all buffered journal transactions during unmount.
    pub fn umount_commit(&mut self) -> Ext4Result<()> {
        if !self.journal_use {
            return Ok(());
        }

        if let Some(system) = self.system.as_mut() {
            let committed =
                system.commit_transaction_with_mapping(&mut self.inner, &self.journal_blocks)?;
            if committed {
                self.inner.invalidate_cache()?;
            }
        } else {
            return Err(Ext4Error::journal_aborted().with_operation("jbd2:commit_without_state"));
        }
        Ok(())
    }

    /// Writes the current internal block buffer.
    pub fn write_block(&mut self, block_id: AbsoluteBN, is_metadata: bool) -> Ext4Result<()> {
        if !self.journal_use || !is_metadata {
            return self.inner.write_block(block_id);
        }

        let new_buf = self.inner.buffer().to_vec().into_boxed_slice();
        let updates = Jbd2Update(block_id, new_buf);

        let Some(system) = self.system.as_mut() else {
            return Err(Ext4Error::journal_aborted().with_operation("jbd2:write_without_state"));
        };
        if Self::enqueue_journal_update(system, &mut self.inner, &self.journal_blocks, updates)? {
            self.inner.invalidate_cache()?;
        }

        Ok(())
    }

    /// Reads one block through the cached inner device.
    pub fn read_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.journal_use
            && let Some(system) = self.system.as_ref()
            && let Some(update) = system
                .commit_queue
                .iter()
                .find(|queued| queued.0 == block_id)
        {
            self.inner.cache_clean_block(block_id, &update.1[..])?;
            return Ok(());
        }

        self.inner.read_block(block_id)
    }

    /// Returns the cached block buffer.
    pub fn buffer(&self) -> &[u8] {
        self.inner.buffer()
    }

    /// Returns the cached block buffer mutably.
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.inner.buffer_mut()
    }

    /// Reads multiple blocks directly.
    pub fn read_blocks(
        &mut self,
        buf: &mut [u8],
        block_id: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        if !self.journal_use || count == 0 {
            return self.inner.read_blocks(buf, block_id, count);
        }

        let block_size = self.inner.block_size() as usize;
        let required = block_size * count as usize;
        if buf.len() < required {
            return Err(Ext4Error::buffer_too_small(buf.len(), required));
        }

        self.inner.read_blocks(buf, block_id, count)?;

        let Some(system) = self.system.as_ref() else {
            return Ok(());
        };
        for i in 0..count {
            let bid = block_id.checked_add(i)?;
            if let Some(update) = system.commit_queue.iter().find(|queued| queued.0 == bid) {
                if update.1.len() != block_size {
                    return Err(Ext4Error::corrupted().with_operation("jbd2:update_block_size"));
                }
                let off = (i as usize) * block_size;
                buf[off..off + block_size].copy_from_slice(&update.1);
            }
        }
        Ok(())
    }

    /// Writes multiple blocks, optionally journaling metadata buffers.
    pub fn write_blocks(
        &mut self,
        buf: &[u8],
        block_id: AbsoluteBN,
        count: u32,
        is_metadata: bool,
    ) -> Ext4Result<()> {
        if !self.journal_use || !is_metadata {
            return self.inner.write_blocks(buf, block_id, count);
        }

        let Some(system) = self.system.as_mut() else {
            return Err(Ext4Error::journal_aborted().with_operation("jbd2:write_without_state"));
        };
        let block_size = self.inner.block_size() as usize;
        let required = count as usize * block_size;
        if buf.len() < required {
            return Err(Ext4Error::buffer_too_small(buf.len(), required));
        }

        let mut committed_any = false;
        for i in 0..count {
            let off = (i as usize) * block_size;
            let boxbuf = buf[off..off + block_size].to_vec().into_boxed_slice();
            let updates = Jbd2Update(block_id.checked_add(i)?, boxbuf);

            committed_any |= Self::enqueue_journal_update(
                system,
                &mut self.inner,
                &self.journal_blocks,
                updates,
            )?;
        }
        if committed_any {
            self.inner.invalidate_cache()?;
        }

        Ok(())
    }

    /// Flushes the inner cached device.
    pub fn flush(&mut self) -> Ext4Result<()> {
        self.inner.flush()
    }

    /// Flushes the inner cached device using the original misspelled API name.
    pub fn cantflush(&mut self) -> Ext4Result<()> {
        self.flush()
    }

    /// Returns the total number of device blocks.
    pub fn total_blocks(&self) -> u64 {
        self.inner.total_blocks()
    }

    /// Returns the underlying device block size.
    pub fn block_size(&self) -> u32 {
        self.inner.block_size()
    }
}

impl<B: BlockIo + Clock> Clock for Jbd2Dev<B> {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        self.inner._device().now()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::config::BLOCK_SIZE;

    struct MemBlockDev {
        data: Vec<u8>,
        fail_flush: bool,
        fail_write_block: Option<AbsoluteBN>,
    }

    impl MemBlockDev {
        fn new(blocks: usize) -> Self {
            Self {
                data: vec![0; blocks * BLOCK_SIZE],
                fail_flush: false,
                fail_write_block: None,
            }
        }

        fn with_failing_flush(blocks: usize) -> Self {
            Self {
                data: vec![0; blocks * BLOCK_SIZE],
                fail_flush: true,
                fail_write_block: None,
            }
        }

        fn with_failing_write_block(blocks: usize, block: AbsoluteBN) -> Self {
            Self {
                data: vec![0; blocks * BLOCK_SIZE],
                fail_flush: false,
                fail_write_block: Some(block),
            }
        }
    }

    impl BlockIo for MemBlockDev {
        fn read(
            &mut self,
            buffer: &mut [u8],
            block_id: crate::io::SectorId,
            _count: u32,
        ) -> Ext4Result<()> {
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
            if self
                .fail_write_block
                .is_some_and(|block| crate::io::SectorId::new(block.raw()) == block_id)
            {
                return Err(Ext4Error::io());
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + buffer.len();
            self.data[start..end].copy_from_slice(buffer);
            Ok(())
        }

        fn flush(&mut self) -> Ext4Result<()> {
            if self.fail_flush {
                Err(Ext4Error::io())
            } else {
                Ok(())
            }
        }

        fn geometry(&self) -> crate::io::DeviceGeometry {
            crate::io::DeviceGeometry::new(BLOCK_SIZE as u32, {
                (self.data.len() / BLOCK_SIZE) as u64
            })
        }

        fn capabilities(&self) -> crate::io::DeviceCapabilities {
            crate::io::DeviceCapabilities {
                read_only: { false },

                flush: true,

                ..crate::io::DeviceCapabilities::default()
            }
        }
    }

    impl crate::runtime::Clock for MemBlockDev {
        fn now(&self) -> Ext4Result<Ext4Timestamp> {
            Ok(Ext4Timestamp::new(0, 0))
        }
    }

    #[test]
    fn auto_commit_invalidates_stale_block_cache() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(JournalSuperBllockS::default(), AbsoluteBN::new(128));

        let target = AbsoluteBN::new(10);
        dev.read_block(target).expect("prime target cache");
        assert_eq!(dev.buffer()[0], 0);

        let count = (JBD2_BUFFER_MAX + 1) as u32;
        let mut updates = vec![0u8; count as usize * BLOCK_SIZE];
        for idx in 0..count as usize {
            updates[idx * BLOCK_SIZE] = (idx + 1) as u8;
        }

        dev.write_blocks(&updates, target, count, true)
            .expect("queue metadata updates");

        dev.read_block(target)
            .expect("read target after auto commit");
        assert_eq!(dev.buffer()[0], 1);
    }

    #[test]
    fn bulk_read_overlays_pending_journal_update() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(JournalSuperBllockS::default(), AbsoluteBN::new(128));

        let target = AbsoluteBN::new(10);
        let pending = vec![0x5a; BLOCK_SIZE];
        dev.write_blocks(&pending, target, 1, true)
            .expect("queue metadata update");

        let mut observed = vec![0; BLOCK_SIZE];
        dev.read_blocks(&mut observed, target, 1)
            .expect("bulk read pending metadata");

        assert_eq!(observed, pending);
    }

    #[test]
    fn metadata_write_never_bypasses_uninitialized_journal() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
        let target = AbsoluteBN::new(3);
        dev.read_block(target).expect("prime target buffer");
        dev.buffer_mut()[0] = 0x5a;

        let error = dev
            .write_block(target, true)
            .expect_err("metadata write must require initialized journal state");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);

        let inner = dev.into_inner();
        assert_eq!(inner.data[target.as_usize().unwrap() * BLOCK_SIZE], 0);
    }

    #[test]
    fn unmount_commit_requires_initialized_journal() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
        let error = dev
            .umount_commit()
            .expect_err("journal-enabled unmount cannot claim a successful commit without state");

        assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);
    }

    #[test]
    fn journal_superblock_must_match_filesystem_block_size() {
        let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
        let mut superblock = JournalSuperBllockS::default();
        superblock.s_blocksize = 1024;

        let error = dev
            .validate_journal_superblock(&superblock, superblock.s_maxlen as usize)
            .expect_err("journal and filesystem block sizes must match");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::BadSuperblock);
    }

    #[test]
    fn journal_superblock_checksum_is_verified_before_use() {
        let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
        let mut superblock = JournalSuperBllockS::default();
        superblock.s_checksum_type = JBD2_CRC32C_CHKSUM;
        crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
        superblock.s_checksum ^= 1;

        let error = dev
            .validate_journal_superblock(&superblock, superblock.s_maxlen as usize)
            .expect_err("damaged journal checksum must be rejected");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::ChecksumMismatch);
    }

    #[test]
    fn umount_commit_propagates_device_flush_failure() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush(256), true);
        dev.set_journal_superblock(JournalSuperBllockS::default(), AbsoluteBN::new(128));
        dev.write_block(AbsoluteBN::new(10), true)
            .expect("queue metadata update");

        let error = dev
            .umount_commit()
            .expect_err("unmount commit must propagate the device error");

        assert_eq!(error, Ext4Error::io());
    }

    #[test]
    fn umount_commit_returns_journal_superblock_write_failure_without_panicking() {
        let journal_superblock = AbsoluteBN::new(128);
        let mut dev = Jbd2Dev::initial_jbd2dev(
            0,
            MemBlockDev::with_failing_write_block(256, journal_superblock),
            true,
        );
        dev.set_journal_superblock(JournalSuperBllockS::default(), journal_superblock);
        dev.write_block(AbsoluteBN::new(10), true)
            .expect("queue metadata update");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dev.umount_commit()));

        assert!(result.is_ok(), "journal I/O failure must not panic");
        assert_eq!(result.unwrap(), Err(Ext4Error::io()));
    }

    #[test]
    fn umount_commit_returns_cache_invalidation_failure_without_panicking() {
        let cached_block = AbsoluteBN::new(20);
        let mut dev = Jbd2Dev::initial_jbd2dev(
            0,
            MemBlockDev::with_failing_write_block(256, cached_block),
            true,
        );
        dev.set_journal_superblock(JournalSuperBllockS::default(), AbsoluteBN::new(128));
        dev.read_block(cached_block).expect("prime cached block");
        dev.buffer_mut()[0] = 1;
        dev.write_block(AbsoluteBN::new(10), true)
            .expect("queue metadata update");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dev.umount_commit()));

        assert!(result.is_ok(), "cache invalidation failure must not panic");
        assert_eq!(result.unwrap(), Err(Ext4Error::io()));
    }

    #[test]
    fn rejects_an_empty_journal_mapping() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);

        let error = dev
            .set_journal_superblock_with_mapping(JournalSuperBllockS::default(), Vec::new())
            .expect_err("empty journal mappings are corrupt");

        assert_eq!(error, Ext4Error::corrupted());
        assert_eq!(dev.journal_sequence(), None);
    }
}
