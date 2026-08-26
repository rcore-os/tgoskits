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
    /// Refreshes a queued update from the held buffer before it can be flushed.
    ///
    /// Editing a block after it enters `commit_queue` makes the held copy dirty
    /// again. The journal still owns writeback for that block, so crossing a
    /// held-buffer boundary must update the queued snapshot instead of exposing
    /// the edit at its home location before the commit record is durable.
    fn refresh_pending_held_update(&mut self) {
        if !self.journal_use {
            return;
        }

        let Some(block_id) = self.inner.dirty_held_block_id() else {
            return;
        };
        let Some(update) = self.system.as_mut().and_then(|system| {
            system
                .commit_queue
                .iter_mut()
                .find(|queued| queued.0 == block_id)
        }) else {
            return;
        };

        update.1.copy_from_slice(self.inner.buffer());
        self.inner.acknowledge_journaled_block(block_id);
        trace!("[JBD2 buffer] refreshed pending metadata block {block_id}");
    }

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
        let _ = self.journal_replay_checked();
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

        self.refresh_pending_held_update();

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
    pub fn umount_commit(&mut self) -> Ext4Result<()> {
        if !self.journal_use {
            trace!("Journal disabled, skip commit");
            return Ok(());
        }

        self.refresh_pending_held_update();

        if let Some(system) = self.system.as_mut() {
            let committed = system
                .commit_transaction_with_mapping(self.inner.device_mut(), &self.journal_blocks)?;
            if committed {
                self.inner.invalidate_cache()?;
            }
        } else {
            trace!("Journal enabled but system uninitialized, skip commit");
        }
        Ok(())
    }

    /// Writes the current internal block buffer.
    pub fn write_block(&mut self, block_id: AbsoluteBN, is_metadata: bool) -> Ext4Result<()> {
        if !self.journal_use || !is_metadata {
            return self.inner.write_block(block_id);
        }

        self.refresh_pending_held_update();
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
        let committed = Self::enqueue_journal_update(system, self.inner.device_mut(), updates)?;
        self.inner.acknowledge_journaled_block(block_id);
        if committed {
            self.inner.invalidate_cache()?;
        }
        trace!("[JBD2 buffer] queued metadata block {block_id}");
        Ok(())
    }

    /// Reads one block through the cached inner device.
    pub fn read_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.inner.holds_block(block_id) {
            return Ok(());
        }
        self.refresh_pending_held_update();
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

        let required = BLOCK_SIZE * count as usize;
        if buf.len() < required {
            return Err(Ext4Error::buffer_too_small(buf.len(), required));
        }

        self.refresh_pending_held_update();

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

        let required = count as usize * BLOCK_SIZE;
        if buf.len() < required {
            return Err(Ext4Error::buffer_too_small(buf.len(), required));
        }

        self.refresh_pending_held_update();
        let Some(system) = self.system.as_mut() else {
            error!(
                "journal is enabled but JBD2 state is not initialized; writing {count} block(s) \
                 starting at {block_id} directly"
            );
            return self.inner.write_blocks(buf, block_id, count);
        };

        let mut committed_any = false;
        for i in 0..count {
            let off = (i as usize) * BLOCK_SIZE;
            let journaled_block = block_id.checked_add(i)?;
            let data: &[u8; BLOCK_SIZE] = buf[off..off + BLOCK_SIZE]
                .try_into()
                .map_err(|_| Ext4Error::buffer_too_small(buf.len(), required))?;
            let mut boxbuf = Box::new([0; BLOCK_SIZE]);
            boxbuf[..].copy_from_slice(data);
            let updates = Jbd2Update(journaled_block, boxbuf);

            committed_any |=
                Self::enqueue_journal_update(system, self.inner.device_mut(), updates)?;
            self.inner
                .acknowledge_journaled_block_with(journaled_block, data);
        }
        if committed_any {
            self.inner.invalidate_cache()?;
        }

        Ok(())
    }

    /// Flushes the inner cached device.
    pub fn flush(&mut self) -> Ext4Result<()> {
        self.refresh_pending_held_update();
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

    /// Returns the current timestamp from the underlying device.
    pub fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
        self.inner._device().current_time()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    struct MemBlockDev {
        data: Vec<u8>,
        writes: Vec<AbsoluteBN>,
        fail_flush: bool,
        fail_write_block: Option<AbsoluteBN>,
    }

    impl MemBlockDev {
        fn new(blocks: usize) -> Self {
            Self {
                data: vec![0; blocks * BLOCK_SIZE],
                writes: Vec::new(),
                fail_flush: false,
                fail_write_block: None,
            }
        }

        fn with_failing_flush(blocks: usize) -> Self {
            Self {
                data: vec![0; blocks * BLOCK_SIZE],
                writes: Vec::new(),
                fail_flush: true,
                fail_write_block: None,
            }
        }

        fn with_failing_write_block(blocks: usize, block: AbsoluteBN) -> Self {
            Self {
                data: vec![0; blocks * BLOCK_SIZE],
                writes: Vec::new(),
                fail_flush: false,
                fail_write_block: Some(block),
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
            if self.fail_write_block == Some(block_id) {
                return Err(Ext4Error::io());
            }
            self.writes.push(block_id);
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

        fn flush(&mut self) -> Ext4Result<()> {
            if self.fail_flush {
                Err(Ext4Error::io())
            } else {
                Ok(())
            }
        }

        fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
            Ok(Ext4Timestamp::new(0, 0))
        }
    }

    #[test]
    fn queued_metadata_stays_off_home_block_until_commit() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(JournalSuperBllockS::default(), AbsoluteBN::new(128));

        let home_block = AbsoluteBN::new(10);
        dev.read_block(home_block).expect("read metadata block");
        dev.buffer_mut()[0] = 0xa5;
        dev.write_block(home_block, true)
            .expect("queue metadata update");
        dev.read_block(AbsoluteBN::new(11))
            .expect("switch held metadata block");

        let raw = dev.into_inner();
        assert!(
            !raw.writes.contains(&home_block),
            "queued metadata reached its home block before the JBD2 commit record"
        );
    }

    #[test]
    fn switching_from_reedited_pending_metadata_refreshes_journal_snapshot() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(JournalSuperBllockS::default(), AbsoluteBN::new(128));

        let home_block = AbsoluteBN::new(10);
        dev.read_block(home_block).expect("read metadata block");
        dev.buffer_mut()[0] = 0xa5;
        dev.write_block(home_block, true)
            .expect("queue metadata update");
        dev.buffer_mut()[0] = 0x5a;
        dev.read_block(AbsoluteBN::new(11))
            .expect("switch away from reedited metadata");

        assert_eq!(
            dev.system
                .as_ref()
                .expect("journal system")
                .commit_queue
                .iter()
                .find(|update| update.0 == home_block)
                .expect("pending home-block update")
                .1[0],
            0x5a,
            "switching blocks must refresh the pending journal snapshot"
        );
        let raw = dev.into_inner();
        assert!(
            !raw.writes.contains(&home_block),
            "reedited pending metadata reached home before the JBD2 commit record"
        );
    }

    #[test]
    fn flushing_reedited_pending_metadata_refreshes_journal_snapshot() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(JournalSuperBllockS::default(), AbsoluteBN::new(128));

        let home_block = AbsoluteBN::new(10);
        dev.read_block(home_block).expect("read metadata block");
        dev.buffer_mut()[0] = 0xa5;
        dev.write_block(home_block, true)
            .expect("queue metadata update");
        dev.buffer_mut()[0] = 0x5a;
        dev.flush().expect("flush underlying journal device");

        assert_eq!(
            dev.system
                .as_ref()
                .expect("journal system")
                .commit_queue
                .iter()
                .find(|update| update.0 == home_block)
                .expect("pending home-block update")
                .1[0],
            0x5a,
            "flushing must refresh the pending journal snapshot"
        );
        let raw = dev.into_inner();
        assert!(
            !raw.writes.contains(&home_block),
            "flushed pending metadata reached home before the JBD2 commit record"
        );
    }

    #[test]
    fn auto_commit_refreshes_reedited_pending_metadata() {
        let journal_start = AbsoluteBN::new(128);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(JournalSuperBllockS::default(), journal_start);

        let home_block = AbsoluteBN::new(10);
        dev.read_block(home_block).expect("read metadata block");
        dev.buffer_mut()[0] = 0xa5;
        dev.write_block(home_block, true)
            .expect("queue initial metadata update");

        let fill_count = JBD2_BUFFER_MAX - 1;
        let fill = vec![0x11; fill_count * BLOCK_SIZE];
        dev.write_blocks(&fill, AbsoluteBN::new(20), fill_count as u32, true)
            .expect("fill journal queue without committing");
        assert_eq!(
            dev.system
                .as_ref()
                .expect("journal system")
                .commit_queue
                .len(),
            JBD2_BUFFER_MAX
        );

        dev.buffer_mut()[0] = 0x5a;
        assert!(
            !dev.inner._device().writes.contains(&home_block),
            "reedited pending metadata reached home before the automatic commit"
        );

        dev.write_block(AbsoluteBN::new(40), true)
            .expect("trigger automatic journal commit");

        let raw = dev.into_inner();
        let first_payload_block = journal_start
            .checked_add(2)
            .expect("first journal payload block")
            .as_usize()
            .expect("journal payload block index");
        assert_eq!(
            raw.data[first_payload_block * BLOCK_SIZE],
            0x5a,
            "automatic commit must journal the latest held metadata snapshot"
        );
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
