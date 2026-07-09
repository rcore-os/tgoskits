//! JBD2-aware block device facade.

use alloc::{boxed::Box, vec::Vec};

use log::{error, trace, warn};

use super::{cached_device::BlockDev, traits::BlockDevice};
use crate::{
    bmalloc::AbsoluteBN,
    config::{BLOCK_SIZE, JBD2_BUFFER_MAX},
    disknode::Ext4Timestamp,
    error::{Ext4Error, Ext4Result},
    jbd2::{
        jbd2::ReplayStatus,
        jbdstruct::{JBD2DEVSYSTEM, Jbd2Update, JournalSuperBllockS},
    },
};

/// Runtime state of the journal proxy.
pub enum Jbd2RunState {
    Commit,
    Replay,
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    struct MemBlockDev {
        data: Vec<u8>,
    }

    impl MemBlockDev {
        fn new(blocks: usize) -> Self {
            Self {
                data: vec![0; blocks * BLOCK_SIZE],
            }
        }
    }

    impl BlockDevice for MemBlockDev {
        fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + buffer.len();
            buffer.copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, _count: u32) -> Ext4Result<()> {
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + buffer.len();
            self.data[start..end].copy_from_slice(buffer);
            Ok(())
        }

        fn open(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn close(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn total_blocks(&self) -> u64 {
            (self.data.len() / BLOCK_SIZE) as u64
        }

        fn block_size(&self) -> u32 {
            BLOCK_SIZE as u32
        }

        fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
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
}

/// Block device proxy that optionally routes metadata writes through JBD2.
pub struct Jbd2Dev<B: BlockDevice> {
    _mode: u8,
    inner: BlockDev<B>,
    journal_use: bool,
    _state: Jbd2RunState,
    system: Option<JBD2DEVSYSTEM>,
    journal_blocks: Vec<AbsoluteBN>,
}

impl<B: BlockDevice> Jbd2Dev<B> {
    fn enqueue_journal_update(
        system: &mut JBD2DEVSYSTEM,
        raw_dev: &mut B,
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
            system.commit_transaction(raw_dev)?;
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

    /// Returns whether journal support is enabled.
    pub fn is_use_journal(&self) -> bool {
        self.journal_use
    }

    /// Returns the current journal transaction sequence if journal is active.
    pub fn journal_sequence(&self) -> Option<u32> {
        self.system.as_ref().map(|s| s.sequence)
    }

    /// Replays the journal if the proxy is configured to use it.
    pub fn journal_replay(&mut self) {
        let status = self.journal_replay_checked();
        if matches!(status, ReplayStatus::Incomplete) {
            warn!("journal replay incomplete — filesystem may be inconsistent");
        }
    }

    /// Replays the journal if JBD2 state is available.
    ///
    /// Returning `Incomplete` here is intentionally conservative: callers that
    /// need recovery correctness should abort rather than continue with direct
    /// writes when the filesystem advertises a journal but no journal state was
    /// installed.
    pub(crate) fn journal_replay_checked(&mut self) -> ReplayStatus {
        if !self.journal_use {
            warn!("journal replay requested while journaling is disabled");
            return ReplayStatus::Complete;
        }

        let Some(jbd_sys) = self.system.as_mut() else {
            error!("journal replay requested before JBD2 state was initialized");
            return ReplayStatus::Incomplete;
        };

        let status = jbd_sys.replay_with_mapping(self.inner.device_mut(), &self.journal_blocks);
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
        self.journal_blocks = journal_blocks;
        self.system = Some(Self::make_system(super_block, journal_start_block));
        Ok(())
    }

    /// Commits all buffered journal transactions during unmount.
    pub fn umount_commit(&mut self) {
        if !self.journal_use {
            trace!("Journal disabled, skip commit");
            return;
        }

        if let Some(system) = self.system.as_mut() {
            system
                .commit_transaction_with_mapping(self.inner.device_mut(), &self.journal_blocks)
                .expect("journal transaction commit failed");
            // The commit checkpoint writes blocks directly to the raw
            // device, bypassing the 4-entry LRU.  Invalidate the LRU so
            // subsequent reads go to disk instead of serving stale data.
            // invalidate_cache() flushes dirty entries before clearing them,
            // so its Result carries a real write-back error; surface it loudly
            // (this path returns `()`, matching the commit's `.expect()` above).
            self.inner
                .invalidate_cache()
                .expect("cache invalidation after unmount commit failed");
        } else {
            trace!("Journal enabled but system uninitialized, skip commit");
        }
    }

    /// Writes the current internal block buffer.
    pub fn write_block(&mut self, block_id: AbsoluteBN, is_metadata: bool) -> Ext4Result<()> {
        if !self.journal_use || !is_metadata {
            return self.inner.write_block(block_id);
        }

        let meta_vec = self.inner.buffer();
        let mut new_buf = Box::new([0; BLOCK_SIZE]);
        new_buf[..].copy_from_slice(meta_vec);
        let updates = Jbd2Update(block_id, new_buf);

        let Some(system) = self.system.as_mut() else {
            error!(
                "journal is enabled but JBD2 state is not initialized; writing block {block_id} \
                 directly"
            );
            return self.inner.write_block(block_id);
        };
        let raw_dev = self.inner.device_mut();

        let old_len = system.commit_queue.len();
        Self::enqueue_journal_update(system, raw_dev, updates)?;
        // If the journal committed (queue shrank), the commit checkpoint
        // wrote blocks directly to the raw device, bypassing the 4-entry
        // LRU. Invalidate the LRU so subsequent reads see fresh data.
        if system.commit_queue.len() < old_len {
            self.inner.invalidate_cache()?;
        }
        trace!("[JBD2 buffer] queued metadata block {block_id}");
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
            self.inner.cache_clean_block(block_id, &update.1)?;
            return Ok(());
        }

        self.inner.read_block(block_id)
    }

    /// Reads one block directly into `buffer`, bypassing the cached LRU.
    /// Still checks the journal commit queue for pending metadata updates.
    pub fn read_block_direct(&mut self, buffer: &mut [u8], block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.journal_use
            && let Some(system) = self.system.as_ref()
            && let Some(update) = system
                .commit_queue
                .iter()
                .find(|queued| queued.0 == block_id)
        {
            if buffer.len() < BLOCK_SIZE {
                return Err(Ext4Error::buffer_too_small(buffer.len(), BLOCK_SIZE));
            }
            buffer[..BLOCK_SIZE].copy_from_slice(&update.1[..BLOCK_SIZE]);
            return Ok(());
        }

        self.inner.read_block_direct(buffer, block_id)
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
    ///
    /// Checks the journal commit queue for each block in the range, matching
    /// the behaviour of [`read_block_direct`].  Without this check a read that
    /// falls between a metadata `write_blocks(is_metadata: true)` and the
    /// journal commit sees stale on-disk data, which causes the read-modify-
    /// write in the inode/bitmap cache write helpers to build a buffer that
    /// silently drops prior modifications to the same block.
    pub fn read_blocks(
        &mut self,
        buf: &mut [u8],
        block_id: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        if !self.journal_use || count == 0 {
            return self.inner.read_blocks(buf, block_id, count);
        }

        let required = BLOCK_SIZE * count as usize;
        if buf.len() < required {
            return Err(Ext4Error::buffer_too_small(buf.len(), required));
        }

        // Bulk-read the whole range in a single device round-trip, then overlay
        // any block that has a pending journal update so the read reflects the
        // not-yet-committed metadata.  A read that falls between a metadata
        // `write_blocks(is_metadata: true)` and the journal commit would
        // otherwise see stale on-disk data, which causes the read-modify-write
        // in the inode/bitmap cache write helpers to build a buffer that
        // silently drops prior modifications to the same block.
        self.inner.read_blocks(buf, block_id, count)?;

        let Some(system) = self.system.as_ref() else {
            return Ok(());
        };
        for i in 0..count {
            let bid = block_id.checked_add(i)?;
            if let Some(update) = system.commit_queue.iter().find(|queued| queued.0 == bid) {
                let off = (i as usize) * BLOCK_SIZE;
                buf[off..off + BLOCK_SIZE].copy_from_slice(&update.1[..BLOCK_SIZE]);
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
            error!(
                "journal is enabled but JBD2 state is not initialized; writing {count} block(s) \
                 starting at {block_id} directly"
            );
            return self.inner.write_blocks(buf, block_id, count);
        };
        let raw_dev = self.inner.device_mut();
        let required = count as usize * BLOCK_SIZE;
        if buf.len() < required {
            return Err(Ext4Error::buffer_too_small(buf.len(), required));
        }

        // Validate the whole block range up front so a later overflow cannot
        // leave earlier blocks committed while the call still reports failure.
        block_id.checked_add(count.saturating_sub(1))?;

        // Track whether a journal commit happened during the loop.
        // `enqueue_journal_update` commits when the queue fills up,
        // writing blocks directly to the raw device and bypassing the
        // 4-entry LRU. We must invalidate the LRU whenever this occurs,
        // not just when the final queue is shorter than the initial queue.
        let mut commit_occurred = false;
        for i in 0..count {
            let off = (i as usize) * BLOCK_SIZE;
            let mut boxbuf = Box::new([0; BLOCK_SIZE]);
            boxbuf[..].copy_from_slice(&buf[off..off + BLOCK_SIZE]);
            let updates = Jbd2Update(block_id.checked_add(i)?, boxbuf);

            let before = system.commit_queue.len();
            Self::enqueue_journal_update(system, raw_dev, updates)?;
            if system.commit_queue.len() < before {
                commit_occurred = true;
            }
        }
        if commit_occurred {
            self.inner.invalidate_cache()?;
        }

        Ok(())
    }

    /// Flushes the inner cached device.
    pub fn cantflush(&mut self) -> Ext4Result<()> {
        self.inner.flush()
    }

    /// Returns the total number of device blocks.
    pub fn total_blocks(&self) -> u64 {
        self.inner.total_blocks()
    }

    /// Returns the underlying device block size.
    pub fn block_size(&self) -> u32 {
        self.inner.block_size()
    }

    /// Returns the current timestamp from the underlying device.
    pub fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        self.inner._device().current_time()
    }
}
