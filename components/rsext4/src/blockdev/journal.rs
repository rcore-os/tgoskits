//! JBD2-aware block device facade.

use alloc::{boxed::Box, vec::Vec};

use log::{error, trace, warn};

use super::{cached_device::BlockDev, traits::BlockDevice};
use crate::{
    bmalloc::AbsoluteBN,
    config::{BLOCK_SIZE, JBD2_BUFFER_MAX},
    disknode::Ext4Timestamp,
    error::Ext4Result,
    jbd2::jbdstruct::{JBD2DEVSYSTEM, Jbd2Update, JournalSuperBllockS},
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
    systeam: Option<JBD2DEVSYSTEM>,
}

impl<B: BlockDevice> Jbd2Dev<B> {
    fn enqueue_journal_update(
        systeam: &mut JBD2DEVSYSTEM,
        raw_dev: &mut B,
        update: Jbd2Update,
    ) -> Ext4Result<()> {
        if let Some(existing) = systeam
            .commit_queue
            .iter_mut()
            .find(|queued| queued.0 == update.0)
        {
            *existing = update;
            return Ok(());
        }

        if systeam.commit_queue.len() >= JBD2_BUFFER_MAX {
            systeam.commit_transaction(raw_dev)?;
        }

        systeam.commit_queue.push(update);
        Ok(())
    }

    /// Creates a new JBD2 block device proxy.
    pub fn initial_jbd2dev(_mode: u8, block_dev: B, use_journal: bool) -> Self {
        let block_dev = BlockDev::new(block_dev);
        Self {
            _mode,
            inner: block_dev,
            journal_use: use_journal,
            _state: Jbd2RunState::Commit,
            systeam: None,
        }
    }

    /// Returns whether journal support is enabled.
    pub fn is_use_journal(&self) -> bool {
        self.journal_use
    }

    /// Returns the current journal transaction sequence if journal is active.
    pub fn journal_sequence(&self) -> Option<u32> {
        self.systeam.as_ref().map(|s| s.sequence)
    }

    /// Replays the journal if the proxy is configured to use it.
    pub fn journal_replay(&mut self) {
        if self.journal_use {
            let dev = self.inner.device_mut();
            let jbd_sys = &mut self
                .systeam
                .as_mut()
                .expect("jbd2dev are not initial,please initial the jbd2dev first!");
            jbd_sys.replay(dev);
        } else {
            warn!("Jouranl function not turn ,please turn on this function and retry!");
        }
    }

    /// Enables or disables journal use at runtime.
    pub fn set_journal_use(&mut self, use_journal: bool) {
        self.journal_use = use_journal;
    }

    /// Installs the journal superblock so JBD2 state can be initialized lazily.
    pub fn set_journal_superblock(
        &mut self,
        super_block: JournalSuperBllockS,
        jouranl_start_block: AbsoluteBN,
    ) {
        let system = JBD2DEVSYSTEM {
            start_block: jouranl_start_block,
            max_len: super_block.s_maxlen,
            head: 0,
            sequence: super_block.s_sequence,
            jbd2_super_block: super_block,
            commit_queue: Vec::new(),
        };
        self.systeam = Some(system);
    }

    /// Commits all buffered journal transactions during unmount.
    pub fn umount_commit(&mut self) {
        if !self.journal_use {
            trace!("Journal disabled, skip commit");
            return;
        }

        if let Some(system) = self.systeam.as_mut() {
            system
                .commit_transaction(self.inner.device_mut())
                .expect("Translation commit failed!!!");
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

        if self.systeam.is_none() {
            error!(
                "Journal systeam uninitial,but journal has turned，this sentence must be once!!!"
            );
            return self.inner.write_block(block_id);
        }

        let systeam = self.systeam.as_mut().unwrap();
        let raw_dev = self.inner.device_mut();

        Self::enqueue_journal_update(systeam, raw_dev, updates)?;
        trace!("[JBD2 BUFFER] queued metadata block {block_id}");
        Ok(())
    }

    /// Reads one block through the cached inner device.
    pub fn read_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
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
        self.inner.read_blocks(buf, block_id, count)
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

        if self.systeam.is_none() {
            error!(
                "Journal systeam uninitial,but journal has turned，this sentence must be once!!!"
            );
            return self.inner.write_blocks(buf, block_id, count);
        }

        let systeam = self.systeam.as_mut().unwrap();
        let raw_dev = self.inner.device_mut();

        for i in 0..count {
            let off = (i as usize) * BLOCK_SIZE;
            let mut boxbuf = Box::new([0; BLOCK_SIZE]);
            boxbuf[..].copy_from_slice(&buf[off..off + BLOCK_SIZE]);
            let updates = Jbd2Update(block_id.checked_add(i)?, boxbuf);

            Self::enqueue_journal_update(systeam, raw_dev, updates)?;
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
