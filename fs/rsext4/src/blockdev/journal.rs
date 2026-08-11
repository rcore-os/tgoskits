//! JBD2-aware block device facade.

use alloc::{boxed::Box, vec::Vec};
use core::mem::size_of;

use super::cached_device::BlockDev;
use crate::{
    bmalloc::AbsoluteBN,
    checksum::jbd2_superblock_csum32,
    disknode::Ext4Timestamp,
    error::{Ext4Error, Ext4Result},
    io::BlockIo,
    jbd2::{
        jbd2::{ReplayFailure, ReplayStatus},
        jbdstruct::{
            JBD2_BLOCKTYPE_SUPERBLOCK_V2, JBD2_CRC32C_CHKSUM, JBD2_DESCRIPTOR_HEADER_SIZE,
            JBD2_FEATURE_INCOMPAT_64BIT, JBD2_FEATURE_INCOMPAT_CSUM_V3, JBD2_MAGIC,
            JBD2_TAG_BLOCKNR_HIGH_SIZE, JBD2_TAG_SIZE, JBD2_TAG3_SIZE, JBD2_UUID_SIZE,
            JBD2DEVSYSTEM, Jbd2Update, JournalSuperBllockS,
        },
    },
    runtime::{Clock, JournalReplayPhase},
};

/// Runtime state of the journal proxy.
pub enum Jbd2RunState {
    Commit,
    Replay,
}

struct ActiveJournalHandle {
    credits: usize,
    touched_blocks: Vec<AbsoluteBN>,
    queue_snapshot: Vec<Jbd2Update>,
}

struct JournalAbortState {
    cause: Ext4Error,
    replay_failure: Option<ReplayFailure>,
    persistence_error: Option<Ext4Error>,
}

type ClockCallback<B> = Box<dyn Fn(&B) -> Ext4Result<Ext4Timestamp> + Send>;

/// Block device proxy that optionally routes metadata writes through JBD2.
pub struct Jbd2Dev<B: BlockIo> {
    _mode: u8,
    inner: BlockDev<B>,
    journal_use: bool,
    _state: Jbd2RunState,
    system: Option<JBD2DEVSYSTEM>,
    journal_blocks: Vec<AbsoluteBN>,
    active_handle: Option<ActiveJournalHandle>,
    abort_state: Option<JournalAbortState>,
    clock: ClockCallback<B>,
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
        let mapped_blocks_u32 = u32::try_from(mapped_blocks).map_err(|_| Ext4Error::overflow())?;
        if super_block.s_maxlen == 0
            || super_block.s_maxlen > mapped_blocks_u32
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
        if super_block.s_feature_compat != 0 || super_block.s_feature_ro_compat != 0 {
            return Err(Ext4Error::unsupported().with_operation("jbd2:features"));
        }
        let has_csum_v3 = super_block.s_feature_incompat & JBD2_FEATURE_INCOMPAT_CSUM_V3 != 0;
        if has_csum_v3 != (super_block.s_checksum_type == JBD2_CRC32C_CHKSUM) {
            return Err(Ext4Error::unsupported().with_operation("jbd2:checksum_features"));
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
        Self::transaction_capacity(super_block, self.inner.block_size() as usize, mapped_blocks)?;
        Ok(())
    }

    fn enqueue_journal_update(
        &mut self,
        update: Jbd2Update,
        transaction_capacity: usize,
    ) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:write_after_abort")?;

        if let Some(handle) = self.active_handle.as_mut() {
            if !handle.touched_blocks.contains(&update.0) {
                if handle.touched_blocks.len() >= handle.credits {
                    return Err(Ext4Error::no_space().with_operation("jbd2:handle_credits"));
                }
                handle.touched_blocks.push(update.0);
            }
            let system = self.system.as_mut().ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:write_without_state")
            })?;
            if let Some(existing) = system
                .commit_queue
                .iter_mut()
                .find(|queued| queued.0 == update.0)
            {
                *existing = update;
            } else {
                system.commit_queue.push(update);
            }
            return Ok(());
        }

        let needs_commit = {
            let system = self.system.as_mut().ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:write_without_state")
            })?;
            if let Some(existing) = system
                .commit_queue
                .iter_mut()
                .find(|queued| queued.0 == update.0)
            {
                *existing = update;
                return Ok(());
            }
            system.commit_queue.len() >= transaction_capacity
        };

        if needs_commit {
            self.commit_pending_transaction()?;
        }

        let system = self.system.as_mut().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:write_without_state")
        })?;
        system.commit_queue.push(update);
        Ok(())
    }

    fn clone_commit_queue(queue: &[Jbd2Update]) -> Vec<Jbd2Update> {
        queue
            .iter()
            .map(|update| Jbd2Update(update.0, update.1.to_vec().into_boxed_slice()))
            .collect()
    }

    fn transaction_capacity(
        superblock: &JournalSuperBllockS,
        block_size: usize,
        mapped_blocks: usize,
    ) -> Ext4Result<usize> {
        let has_csum_v3 = superblock.s_feature_incompat & JBD2_FEATURE_INCOMPAT_CSUM_V3 != 0;
        let has_64bit = superblock.s_feature_incompat & JBD2_FEATURE_INCOMPAT_64BIT != 0;
        let descriptor_tail = usize::from(has_csum_v3) * size_of::<u32>();
        let descriptor_end = block_size
            .checked_sub(descriptor_tail)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:descriptor_capacity"))?;
        let fixed_descriptor_bytes = JBD2_DESCRIPTOR_HEADER_SIZE
            .checked_add(JBD2_UUID_SIZE)
            .ok_or_else(Ext4Error::overflow)?;
        let tag_bytes = if has_csum_v3 {
            JBD2_TAG3_SIZE
        } else {
            JBD2_TAG_SIZE + usize::from(has_64bit) * JBD2_TAG_BLOCKNR_HIGH_SIZE
        };
        let descriptor_capacity = descriptor_end
            .checked_sub(fixed_descriptor_bytes)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:descriptor_capacity"))?
            / tag_bytes;

        let declared_blocks =
            usize::try_from(superblock.s_maxlen).map_err(|_| Ext4Error::overflow())?;
        let first = usize::try_from(superblock.s_first).map_err(|_| Ext4Error::overflow())?;
        let ring_records = declared_blocks
            .min(mapped_blocks)
            .checked_sub(first)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:ring_capacity"))?;
        let ring_capacity = ring_records
            .checked_sub(2)
            .ok_or_else(|| Ext4Error::no_space().with_operation("jbd2:ring_capacity"))?;
        let capacity = descriptor_capacity.min(ring_capacity);
        if capacity == 0 {
            return Err(Ext4Error::no_space().with_operation("jbd2:transaction_capacity"));
        }
        Ok(capacity)
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

    fn with_clock_callback(
        _mode: u8,
        block_dev: B,
        use_journal: bool,
        clock: ClockCallback<B>,
    ) -> Self {
        let block_dev = BlockDev::new(block_dev);
        Self {
            _mode,
            inner: block_dev,
            journal_use: use_journal,
            _state: Jbd2RunState::Commit,
            system: None,
            journal_blocks: Vec::new(),
            active_handle: None,
            abort_state: None,
            clock,
        }
    }

    /// Creates the private journal owner with a separately injected clock.
    pub(crate) fn with_clock<C>(mode: u8, block_dev: B, clock: C, use_journal: bool) -> Self
    where
        C: Clock + Send + 'static,
    {
        Self::with_clock_callback(
            mode,
            block_dev,
            use_journal,
            Box::new(move |_device| clock.now()),
        )
    }

    /// Creates the legacy public journal proxy.
    ///
    /// New mount code must inject `Clock` separately through `Ext4::mount`.
    pub fn initial_jbd2dev(mode: u8, block_dev: B, use_journal: bool) -> Self
    where
        B: Clock,
    {
        Self::with_clock_callback(
            mode,
            block_dev,
            use_journal,
            Box::new(|device| device.now()),
        )
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

    fn journal_abort_cause(&self) -> Option<Ext4Error> {
        self.abort_state.as_ref().map(|state| state.cause)
    }

    fn ensure_not_aborted(&self, operation: &'static str) -> Ext4Result<()> {
        if self.journal_abort_cause().is_some() {
            Err(Ext4Error::journal_aborted().with_operation(operation))
        } else {
            Ok(())
        }
    }

    fn abort_journal(&mut self, cause: Ext4Error) {
        if self.journal_abort_cause().is_some() {
            return;
        }
        self.abort_state = Some(JournalAbortState {
            cause,
            replay_failure: None,
            persistence_error: None,
        });

        let persistence_result = self
            .system
            .as_mut()
            .map(|system| system.record_abort_with_mapping(&mut self.inner, &self.journal_blocks));
        if let Some(Err(error)) = persistence_result
            && let Some(state) = self.abort_state.as_mut()
        {
            state.persistence_error = Some(error);
        }
    }

    fn commit_pending_transaction(&mut self) -> Ext4Result<bool> {
        self.ensure_not_aborted("jbd2:commit_after_abort")?;
        let Some(system) = self.system.as_mut() else {
            return Err(Ext4Error::journal_aborted().with_operation("jbd2:commit_without_state"));
        };
        let result = system.commit_transaction_with_mapping(&mut self.inner, &self.journal_blocks);
        let committed = match result {
            Ok(committed) => committed,
            Err(error) => {
                self.abort_journal(error);
                return Err(error);
            }
        };
        if committed && let Err(error) = self.inner.invalidate_cache() {
            self.abort_journal(error);
            return Err(error);
        }
        Ok(committed)
    }

    fn journal_transaction_capacity(&self) -> Ext4Result<usize> {
        self.ensure_not_aborted("jbd2:capacity_after_abort")?;
        let system = self.system.as_ref().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:capacity_without_state")
        })?;
        let mapped_blocks = if self.journal_blocks.is_empty() {
            let available = self
                .total_blocks()
                .checked_sub(system.start_block.raw())
                .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:mapping_capacity"))?;
            usize::try_from(available).map_err(|_| Ext4Error::overflow())?
        } else {
            self.journal_blocks.len()
        };
        Self::transaction_capacity(
            &system.jbd2_super_block,
            self.inner.block_size() as usize,
            mapped_blocks,
        )
    }

    /// Replays the journal if JBD2 state is available.
    ///
    /// Returning `Incomplete` here is intentionally conservative: callers that
    /// need recovery correctness should abort rather than continue with direct
    /// writes when the filesystem advertises a journal but no journal state was
    /// installed.
    pub(crate) fn journal_replay_checked(&mut self) -> ReplayStatus {
        if let Some(state) = self.abort_state.as_ref() {
            return ReplayStatus::Incomplete(state.replay_failure.unwrap_or_else(|| {
                ReplayFailure::without_restart(JournalReplayPhase::Initialize, state.cause)
            }));
        }
        if !self.journal_use {
            return ReplayStatus::Complete;
        }

        let Some(jbd_sys) = self.system.as_mut() else {
            let failure = ReplayFailure::without_restart(
                JournalReplayPhase::Initialize,
                Ext4Error::journal_aborted().with_operation("jbd2:replay_without_state"),
            );
            self.abort_journal(failure.cause());
            if let Some(state) = self.abort_state.as_mut() {
                state.replay_failure = Some(failure);
            }
            return ReplayStatus::Incomplete(failure);
        };

        let status = jbd_sys.replay_with_mapping(&mut self.inner, &self.journal_blocks);
        if let ReplayStatus::Incomplete(failure) = status {
            self.abort_journal(failure.cause());
            if let Some(state) = self.abort_state.as_mut() {
                state.replay_failure = Some(failure);
            }
            return status;
        }
        if let Err(error) = self.inner.invalidate_cache() {
            let failure = ReplayFailure::without_restart(JournalReplayPhase::Cache, error);
            self.abort_journal(failure.cause());
            if let Some(state) = self.abort_state.as_mut() {
                state.replay_failure = Some(failure);
            }
            return ReplayStatus::Incomplete(failure);
        }
        status
    }

    /// Enables or disables journal use when no transaction is in flight.
    pub fn set_journal_use(&mut self, use_journal: bool) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:change_mode_after_abort")?;
        if !use_journal && self.journal_use {
            if self.active_handle.is_some() {
                return Err(Ext4Error::busy().with_operation("jbd2:disable_with_active_handle"));
            }
            if self
                .system
                .as_ref()
                .is_some_and(|system| !system.commit_queue.is_empty())
            {
                return Err(Ext4Error::busy().with_operation("jbd2:disable_with_pending_commit"));
            }
        }
        self.journal_use = use_journal;
        Ok(())
    }

    /// Installs the journal superblock so JBD2 state can be initialized lazily.
    pub fn set_journal_superblock(
        &mut self,
        super_block: JournalSuperBllockS,
        journal_start_block: AbsoluteBN,
    ) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:reinstall_after_abort")?;
        let available = self
            .total_blocks()
            .checked_sub(journal_start_block.raw())
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:mapping_capacity"))?;
        let mapped_blocks = usize::try_from(available).map_err(|_| Ext4Error::overflow())?;
        self.validate_journal_superblock(&super_block, mapped_blocks)?;
        self.journal_blocks.clear();
        self.system = Some(Self::make_system(super_block, journal_start_block));
        Ok(())
    }

    pub(crate) fn set_journal_superblock_with_mapping(
        &mut self,
        super_block: JournalSuperBllockS,
        journal_blocks: Vec<AbsoluteBN>,
    ) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:reinstall_after_abort")?;
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

    /// Commits every buffered journal update without ending the mount.
    pub fn commit(&mut self) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:unmount_after_abort")?;
        if !self.journal_use {
            return Ok(());
        }
        if self.active_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:commit_with_active_handle"));
        }

        self.commit_pending_transaction()?;
        Ok(())
    }

    /// Commits all buffered journal transactions during unmount.
    pub fn umount_commit(&mut self) -> Ext4Result<()> {
        self.commit()
    }

    /// Runs one metadata operation with a bounded number of queue credits.
    ///
    /// The handle joins this implementation's current in-memory journal queue.
    /// It prevents an automatic commit from splitting the operation and
    /// restores queued metadata images if the operation returns an error. This
    /// is not yet a complete Linux JBD2 handle: the filesystem transaction
    /// owner must also restore its caches and allocation state.
    fn with_journal_handle<T>(
        &mut self,
        credits: usize,
        operation: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        self.ensure_not_aborted("jbd2:handle_after_abort")?;
        if !self.journal_use {
            return operation(self);
        }
        if credits == 0 {
            return Err(Ext4Error::invalid_input().with_operation("jbd2:handle_credits"));
        }
        let transaction_capacity = self.journal_transaction_capacity()?;
        if credits > transaction_capacity {
            return Err(Ext4Error::no_space().with_operation("jbd2:handle_credits"));
        }
        if self.active_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:nested_handle"));
        }

        let needs_commit = {
            let Some(system) = self.system.as_mut() else {
                return Err(
                    Ext4Error::journal_aborted().with_operation("jbd2:handle_without_state")
                );
            };
            let reserved = system
                .commit_queue
                .len()
                .checked_add(credits)
                .ok_or_else(Ext4Error::overflow)?;
            reserved > transaction_capacity
        };
        if needs_commit {
            self.commit_pending_transaction()?;
        }

        let queue_snapshot = {
            let system = self.system.as_ref().ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:handle_without_state")
            })?;
            Self::clone_commit_queue(&system.commit_queue)
        };
        self.active_handle = Some(ActiveJournalHandle {
            credits,
            touched_blocks: Vec::with_capacity(credits),
            queue_snapshot,
        });

        match operation(self) {
            Ok(value) => {
                self.active_handle = None;
                Ok(value)
            }
            Err(operation_error) => {
                let handle = self.active_handle.take().ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("jbd2:missing_active_handle")
                })?;
                let Some(system) = self.system.as_mut() else {
                    return Err(
                        Ext4Error::journal_aborted().with_operation("jbd2:abort_without_state")
                    );
                };
                system.commit_queue = handle.queue_snapshot;
                match self.inner.invalidate_cache() {
                    Ok(()) => Err(operation_error),
                    Err(rollback_error) => {
                        Err(rollback_error.with_operation("rollback:jbd2_handle"))
                    }
                }
            }
        }
    }

    /// Writes the current internal block buffer.
    pub fn write_block(&mut self, block_id: AbsoluteBN, is_metadata: bool) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:write_after_abort")?;
        if !self.journal_use || !is_metadata {
            return self.inner.write_block(block_id);
        }

        let new_buf = self.inner.buffer().to_vec().into_boxed_slice();
        let updates = Jbd2Update(block_id, new_buf);
        let transaction_capacity = self.journal_transaction_capacity()?;

        self.enqueue_journal_update(updates, transaction_capacity)
    }

    /// Drops an uncommitted update for a newly allocated metadata block.
    ///
    /// This is only valid while rolling back a block that has not become
    /// reachable from durable filesystem metadata. Published blocks require a
    /// revoke-aware transaction instead of queue removal.
    pub(crate) fn forget_unpublished_metadata(&mut self, block_id: AbsoluteBN) {
        if let Some(system) = self.system.as_mut() {
            system.commit_queue.retain(|update| update.0 != block_id);
        }
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
        self.ensure_not_aborted("jbd2:write_after_abort")?;
        if !self.journal_use || !is_metadata {
            return self.inner.write_blocks(buf, block_id, count);
        }

        let block_size = self.inner.block_size() as usize;
        let required = usize::try_from(count)
            .map_err(|_| Ext4Error::overflow())?
            .checked_mul(block_size)
            .ok_or_else(Ext4Error::overflow)?;
        if buf.len() < required {
            return Err(Ext4Error::buffer_too_small(buf.len(), required));
        }
        let credits = usize::try_from(count).map_err(|_| Ext4Error::overflow())?;
        let transaction_capacity = self.journal_transaction_capacity()?;
        if self.active_handle.is_none() && credits > 1 && credits <= transaction_capacity {
            return self.with_journal_handle(credits, |device| {
                device.write_blocks(buf, block_id, count, is_metadata)
            });
        }

        for i in 0..count {
            let off = (i as usize) * block_size;
            let boxbuf = buf[off..off + block_size].to_vec().into_boxed_slice();
            let updates = Jbd2Update(block_id.checked_add(i)?, boxbuf);

            self.enqueue_journal_update(updates, transaction_capacity)?;
        }

        Ok(())
    }

    /// Forces the running journal transaction and its checkpoint to storage.
    pub fn flush(&mut self) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:flush_after_abort")?;
        if self.journal_use {
            if self.active_handle.is_some() {
                return Err(Ext4Error::busy().with_operation("jbd2:flush_with_active_handle"));
            }
            if self.commit_pending_transaction()? {
                return Ok(());
            }
        }
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

impl<B: BlockIo> Clock for Jbd2Dev<B> {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        (self.clock)(self.inner._device())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::{
        config::BLOCK_SIZE,
        endian::DiskFormat,
        jbd2::jbdstruct::{
            CommitHeader, JBD2_BLOCKTYPE_REVOKE, JBD2_DESCRIPTOR_HEADER_SIZE, JBD2_TAG3_SIZE,
            JBD2_UUID_SIZE, Jbd2JournalRevokeHeadS, JournalBlockTag3S, JournalHeaderS,
        },
    };

    struct MemBlockDev {
        data: Vec<u8>,
        fail_flush: bool,
        fail_write_block: Option<AbsoluteBN>,
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
                fail_write_block: None,
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
                fail_write_block: None,
                fail_fua: false,
                fail_read_sector: None,
                fail_write_sector: None,
                fua_writes: 0,
            }
        }

        fn with_failing_write_block(blocks: usize, block: AbsoluteBN) -> Self {
            Self {
                data: vec![0; blocks * BLOCK_SIZE],
                fail_flush: false,
                fail_write_block: Some(block),
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
                fail_write_block: None,
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

    fn reference_jbd2_seed(uuid: &[u8; JBD2_UUID_SIZE]) -> u32 {
        reference_crc32c(u32::MAX, uuid)
    }

    fn reference_jbd2_tag_checksum(
        uuid: &[u8; JBD2_UUID_SIZE],
        sequence: u32,
        payload: &[u8],
    ) -> u32 {
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

    fn csum_v3_superblock() -> JournalSuperBllockS {
        let mut superblock = JournalSuperBllockS::default();
        superblock.s_maxlen = 64;
        superblock.s_feature_incompat = JBD2_FEATURE_INCOMPAT_64BIT | JBD2_FEATURE_INCOMPAT_CSUM_V3;
        superblock.s_checksum_type = JBD2_CRC32C_CHKSUM;
        superblock.s_uuid = [0x5a; JBD2_UUID_SIZE];
        crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
        superblock
    }

    fn small_journal_superblock() -> JournalSuperBllockS {
        JournalSuperBllockS {
            s_maxlen: 16,
            s_first: 1,
            ..JournalSuperBllockS::default()
        }
    }

    fn committed_csum_v3_fixture() -> (MemBlockDev, JournalSuperBllockS, AbsoluteBN) {
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
        let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
        inner.data[target_start..target_start + BLOCK_SIZE].fill(0);

        let mut replay_superblock = superblock;
        replay_superblock.s_start = replay_superblock.s_first;
        replay_superblock.s_sequence = 1;
        crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
        replay_superblock.to_disk_bytes(&mut inner.data[128 * BLOCK_SIZE..][..1024]);

        (inner, replay_superblock, target)
    }

    fn replay_csum_v3_fixture(
        inner: MemBlockDev,
        superblock: JournalSuperBllockS,
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
            crate::io::DeviceGeometry::new(BLOCK_SIZE as u32, {
                (self.data.len() / BLOCK_SIZE) as u64
            })
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

    #[test]
    fn auto_commit_invalidates_stale_block_cache() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let sequence = dev.journal_sequence().unwrap();

        let target = AbsoluteBN::new(10);
        dev.read_block(target).expect("prime target cache");
        assert_eq!(dev.buffer()[0], 0);

        let count = u32::try_from(dev.journal_transaction_capacity().unwrap() + 1).unwrap();
        let mut updates = vec![0u8; count as usize * BLOCK_SIZE];
        for idx in 0..count as usize {
            updates[idx * BLOCK_SIZE] = (idx + 1) as u8;
        }

        dev.write_blocks(&updates, target, count, true)
            .expect("queue metadata updates");

        dev.read_block(target)
            .expect("read target after auto commit");
        assert_eq!(dev.buffer()[0], 1);
        assert_eq!(dev.journal_sequence(), Some(sequence.wrapping_add(1)));

        dev.umount_commit().expect("commit final queued update");
        assert_eq!(dev.journal_sequence(), Some(sequence.wrapping_add(2)));
        let inner = dev.into_inner();
        for idx in 0..count as usize {
            let start = (target.as_usize().unwrap() + idx) * BLOCK_SIZE;
            assert_eq!(inner.data[start], (idx + 1) as u8);
        }
    }

    #[test]
    fn bulk_read_overlays_pending_journal_update() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");

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
    fn abort_record_failure_does_not_replace_first_commit_error() {
        let mut dev =
            Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush_and_fua(256), true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install checksummed journal");
        dev.write_block(AbsoluteBN::new(10), true)
            .expect("queue metadata update");

        let first_error = dev
            .umount_commit()
            .expect_err("commit failure must remain the primary error");
        assert_eq!(first_error.kind(), crate::Ext4ErrorKind::Io);
        let state = dev.abort_state.as_ref().expect("journal must be aborted");
        assert_eq!(state.cause.kind(), crate::Ext4ErrorKind::Io);
        assert_eq!(
            state.persistence_error.map(Ext4Error::kind),
            Some(crate::Ext4ErrorKind::Io)
        );
        assert_eq!(dev.inner._device().fua_writes, 1);

        let later_error = dev
            .umount_commit()
            .expect_err("the failed abort record must not allow a retry");
        assert_eq!(later_error.kind(), crate::Ext4ErrorKind::JournalAborted);

        let inner = dev.into_inner();
        let journal_offset = 128 * BLOCK_SIZE;
        let recorded = JournalSuperBllockS::from_disk_bytes(
            &inner.data[journal_offset..journal_offset + 1024],
        );
        assert_eq!(
            recorded.s_errno, 0,
            "a failed FUA write must not claim the abort was recorded"
        );
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
        superblock.s_feature_incompat |= JBD2_FEATURE_INCOMPAT_CSUM_V3;
        superblock.s_checksum_type = JBD2_CRC32C_CHKSUM;
        crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
        superblock.s_checksum ^= 1;

        let error = dev
            .validate_journal_superblock(&superblock, superblock.s_maxlen as usize)
            .expect_err("damaged journal checksum must be rejected");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::ChecksumMismatch);
    }

    #[test]
    fn journal_superblock_requires_csum_v3_and_crc32c_together() {
        let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
        let mut missing_type = JournalSuperBllockS::default();
        missing_type.s_feature_incompat |= JBD2_FEATURE_INCOMPAT_CSUM_V3;
        let error = dev
            .validate_journal_superblock(&missing_type, missing_type.s_maxlen as usize)
            .expect_err("csum-v3 requires CRC32C journal superblock checksums");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Unsupported);

        let mut missing_feature = JournalSuperBllockS::default();
        missing_feature.s_checksum_type = JBD2_CRC32C_CHKSUM;
        crate::checksum::jbd2_update_superblock_checksum(&mut missing_feature);
        let error = dev
            .validate_journal_superblock(&missing_feature, missing_feature.s_maxlen as usize)
            .expect_err("CRC32C journal superblock checksums require csum-v3 support");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Unsupported);
    }

    #[test]
    fn transaction_capacity_follows_descriptor_and_ring_geometry() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        let superblock = csum_v3_superblock();
        dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
            .expect("install csum-v3 journal");

        assert_eq!(dev.journal_transaction_capacity().unwrap(), 61);
        let large_ring = JournalSuperBllockS {
            s_maxlen: 4096,
            ..superblock
        };
        assert_eq!(
            Jbd2Dev::<MemBlockDev>::transaction_capacity(&large_ring, 1024, 4096).unwrap(),
            62
        );

        let small = small_journal_superblock();
        assert_eq!(
            Jbd2Dev::<MemBlockDev>::transaction_capacity(&small, BLOCK_SIZE, 16).unwrap(),
            13
        );
    }

    #[test]
    fn journal_install_rejects_ring_without_payload_capacity() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        let too_small = JournalSuperBllockS {
            s_maxlen: 3,
            s_first: 1,
            ..JournalSuperBllockS::default()
        };

        let error = dev
            .set_journal_superblock_with_mapping(
                too_small,
                (128..131).map(AbsoluteBN::new).collect(),
            )
            .expect_err("descriptor and commit alone leave no payload capacity");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
        assert_eq!(dev.journal_sequence(), None);
    }

    #[test]
    fn csum_v3_commit_emits_linux_tag_and_block_checksums() {
        let (inner, superblock, target) = committed_csum_v3_fixture();
        let descriptor = &inner.data[129 * BLOCK_SIZE..130 * BLOCK_SIZE];
        let tag = JournalBlockTag3S::from_disk_bytes(
            &descriptor[JBD2_DESCRIPTOR_HEADER_SIZE..JBD2_DESCRIPTOR_HEADER_SIZE + JBD2_TAG3_SIZE],
        );
        assert_eq!(tag.t_blocknr, target.raw() as u32);
        assert_eq!(tag.t_blocknr_high, 0);
        assert_eq!(
            tag.t_checksum,
            reference_jbd2_tag_checksum(
                &superblock.s_uuid,
                1,
                &inner.data[130 * BLOCK_SIZE..131 * BLOCK_SIZE],
            )
        );
        let descriptor_checksum =
            u32::from_be_bytes(descriptor[BLOCK_SIZE - 4..].try_into().unwrap());
        assert_eq!(
            descriptor_checksum,
            reference_jbd2_block_checksum(&superblock.s_uuid, descriptor, BLOCK_SIZE - 4,)
        );

        let commit_bytes = &inner.data[131 * BLOCK_SIZE..132 * BLOCK_SIZE];
        let commit = CommitHeader::from_disk_bytes(commit_bytes);
        assert_eq!(commit.h_chksum_type, 0);
        assert_eq!(commit.h_chksum_size, 0);
        assert_eq!(
            commit.h_chksum[0],
            reference_jbd2_block_checksum(&superblock.s_uuid, commit_bytes, 16)
        );
    }

    #[test]
    fn csum_v3_writer_transaction_replays_after_checkpoint_loss() {
        let (inner, superblock, target) = committed_csum_v3_fixture();
        let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
        let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        dev.set_journal_superblock_with_mapping(
            superblock,
            (128..192).map(AbsoluteBN::new).collect(),
        )
        .expect("install writer-produced csum-v3 journal");

        assert_eq!(dev.journal_replay_checked(), ReplayStatus::Complete);
        let inner = dev.into_inner();
        assert!(
            inner.data[target_start..target_start + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0xa5)
        );
    }

    #[test]
    fn replay_descriptor_read_failure_preserves_io_cause() {
        let (mut inner, superblock, _) = committed_csum_v3_fixture();
        inner.fail_next_read_at_block(AbsoluteBN::new(129));
        let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        dev.set_journal_superblock_with_mapping(
            superblock,
            (128..192).map(AbsoluteBN::new).collect(),
        )
        .expect("install writer-produced csum-v3 journal");

        let failure = dev
            .journal_replay_checked()
            .failure()
            .expect("replay must stop when the descriptor cannot be read");
        assert_eq!(failure.phase(), JournalReplayPhase::Scan);
        assert_eq!(failure.restart_rel(), Some(superblock.s_first));
        assert_eq!(failure.cause().kind(), crate::Ext4ErrorKind::Io);
        assert_eq!(failure.persistence_error(), None);
        let state = dev.abort_state.as_ref().expect("replay must abort journal");
        assert_eq!(
            state.cause.kind(),
            crate::Ext4ErrorKind::Io,
            "device I/O must not be collapsed into corruption"
        );
    }

    #[test]
    fn replay_payload_read_and_home_write_failures_keep_replay_phase() {
        let (mut inner, superblock, _) = committed_csum_v3_fixture();
        inner.fail_next_read_at_block(AbsoluteBN::new(130));
        let mut read_failure_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        read_failure_dev
            .set_journal_superblock_with_mapping(
                superblock,
                (128..192).map(AbsoluteBN::new).collect(),
            )
            .expect("install replay fixture for payload read fault");

        let read_failure = read_failure_dev
            .journal_replay_checked()
            .failure()
            .expect("payload read fault must stop replay");
        assert_eq!(read_failure.phase(), JournalReplayPhase::Replay);
        assert_eq!(read_failure.cause().kind(), crate::Ext4ErrorKind::Io);

        let (mut inner, superblock, target) = committed_csum_v3_fixture();
        inner.fail_next_write_at_block(target);
        let mut write_failure_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        write_failure_dev
            .set_journal_superblock_with_mapping(
                superblock,
                (128..192).map(AbsoluteBN::new).collect(),
            )
            .expect("install replay fixture for home write fault");

        let write_failure = write_failure_dev
            .journal_replay_checked()
            .failure()
            .expect("home write fault must stop replay");
        assert_eq!(write_failure.phase(), JournalReplayPhase::Replay);
        assert_eq!(write_failure.cause().kind(), crate::Ext4ErrorKind::Io);
        let inner = write_failure_dev.into_inner();
        let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert!(
            inner.data[target_start..target_start + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    #[test]
    fn replay_persist_failure_is_typed_after_successful_home_write() {
        let (mut inner, superblock, target) = committed_csum_v3_fixture();
        inner.fail_flush = true;
        let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        dev.set_journal_superblock_with_mapping(
            superblock,
            (128..192).map(AbsoluteBN::new).collect(),
        )
        .expect("install replay fixture with persist fault");

        let failure = dev
            .journal_replay_checked()
            .failure()
            .expect("final replay flush fault must fail recovery");
        assert_eq!(failure.phase(), JournalReplayPhase::Persist);
        assert_eq!(failure.cause().kind(), crate::Ext4ErrorKind::Io);
        assert_eq!(failure.persistence_error(), None);
        let inner = dev.into_inner();
        let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert!(
            inner.data[target_start..target_start + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0xa5)
        );
    }

    #[test]
    fn replay_superblock_write_failure_is_a_persist_error() {
        let (mut inner, superblock, target) = committed_csum_v3_fixture();
        inner.fail_next_write_at_block(AbsoluteBN::new(128));
        let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        dev.set_journal_superblock_with_mapping(
            superblock,
            (128..192).map(AbsoluteBN::new).collect(),
        )
        .expect("install replay fixture with superblock write fault");

        let failure = dev
            .journal_replay_checked()
            .failure()
            .expect("replay superblock write fault must fail recovery");
        assert_eq!(failure.phase(), JournalReplayPhase::Persist);
        assert_eq!(failure.cause().kind(), crate::Ext4ErrorKind::Io);
        assert_eq!(failure.persistence_error(), None);
        assert_eq!(dev.inner._device().fua_writes, 1);

        let inner = dev.into_inner();
        let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert!(
            inner.data[target_start..target_start + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0xa5)
        );
    }

    #[test]
    fn replay_failure_keeps_primary_checksum_over_progress_flush_error() {
        let (mut inner, superblock, target) = committed_csum_v3_fixture();
        inner.data[130 * BLOCK_SIZE - 1] ^= 1;
        inner.fail_flush = true;
        let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        dev.set_journal_superblock_with_mapping(
            superblock,
            (128..192).map(AbsoluteBN::new).collect(),
        )
        .expect("install corrupt replay fixture with persist fault");

        let failure = dev
            .journal_replay_checked()
            .failure()
            .expect("checksum and persist faults must fail replay");
        assert_eq!(failure.phase(), JournalReplayPhase::Scan);
        assert_eq!(
            failure.cause().kind(),
            crate::Ext4ErrorKind::ChecksumMismatch
        );
        assert_eq!(
            failure.persistence_error().map(Ext4Error::kind),
            Some(crate::Ext4ErrorKind::Io)
        );
        let state = dev.abort_state.as_ref().expect("replay must abort journal");
        assert_eq!(state.cause.kind(), crate::Ext4ErrorKind::ChecksumMismatch);
        assert_eq!(state.replay_failure, Some(failure));

        let inner = dev.into_inner();
        let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert!(
            inner.data[target_start..target_start + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    fn assert_csum_v3_corruption_is_rejected(corrupt: impl FnOnce(&mut Vec<u8>)) {
        let (mut inner, superblock, target) = committed_csum_v3_fixture();
        corrupt(&mut inner.data);
        let (status, _) = replay_csum_v3_fixture(inner, superblock, target);
        let failure = status.failure().expect("corruption must stop replay");
        assert_eq!(
            failure.cause().kind(),
            crate::Ext4ErrorKind::ChecksumMismatch
        );
    }

    #[test]
    fn csum_v3_replay_rejects_descriptor_payload_and_commit_corruption_before_home_write() {
        assert_csum_v3_corruption_is_rejected(|data| {
            data[130 * BLOCK_SIZE - 1] ^= 1;
        });
        assert_csum_v3_corruption_is_rejected(|data| {
            data[130 * BLOCK_SIZE + 64] ^= 1;
        });
        assert_csum_v3_corruption_is_rejected(|data| {
            data[131 * BLOCK_SIZE + 16] ^= 1;
        });
    }

    #[test]
    fn csum_v3_replay_rejects_corrupt_revoke_tail_before_home_write() {
        let (mut inner, superblock, target) = committed_csum_v3_fixture();
        inner
            .data
            .copy_within(131 * BLOCK_SIZE..132 * BLOCK_SIZE, 132 * BLOCK_SIZE);

        let mut revoke = vec![0u8; BLOCK_SIZE];
        Jbd2JournalRevokeHeadS {
            r_header: JournalHeaderS {
                h_magic: JBD2_MAGIC,
                h_blocktype: JBD2_BLOCKTYPE_REVOKE,
                h_sequence: 1,
            },
            r_count: 16,
        }
        .to_disk_bytes(&mut revoke);
        let checksum = crate::checksum::jbd2_descriptor_block_csum32(&superblock.s_uuid, &revoke)
            .expect("revoke checksum");
        revoke[BLOCK_SIZE - 4..].copy_from_slice(&checksum.to_be_bytes());
        revoke[BLOCK_SIZE - 1] ^= 1;
        inner.data[131 * BLOCK_SIZE..132 * BLOCK_SIZE].copy_from_slice(&revoke);

        let (status, _) = replay_csum_v3_fixture(inner, superblock, target);
        let failure = status.failure().expect("corrupt revoke must stop replay");
        assert_eq!(failure.phase(), JournalReplayPhase::Revoke);
        assert_eq!(
            failure.cause().kind(),
            crate::Ext4ErrorKind::ChecksumMismatch
        );
    }

    #[test]
    fn csum_v3_replay_validates_all_payloads_before_any_home_write() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        let superblock = csum_v3_superblock();
        dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
            .expect("install csum-v3 journal");
        let first_target = AbsoluteBN::new(10);
        let second_target = AbsoluteBN::new(11);
        dev.write_blocks(&vec![0xa5; BLOCK_SIZE * 2], first_target, 2, true)
            .expect("queue two csum-v3 metadata blocks");
        dev.umount_commit().expect("commit csum-v3 metadata");

        let mut inner = dev.into_inner();
        let first_home = first_target.as_usize().unwrap() * BLOCK_SIZE;
        let second_home = second_target.as_usize().unwrap() * BLOCK_SIZE;
        inner.data[first_home..first_home + BLOCK_SIZE].fill(0);
        inner.data[second_home..second_home + BLOCK_SIZE].fill(0);
        inner.data[131 * BLOCK_SIZE + 64] ^= 1;
        let mut replay_superblock = superblock;
        replay_superblock.s_start = replay_superblock.s_first;
        replay_superblock.s_sequence = 1;
        crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
        replay_superblock.to_disk_bytes(&mut inner.data[128 * BLOCK_SIZE..][..1024]);

        let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        replay_dev
            .set_journal_superblock_with_mapping(
                replay_superblock,
                (128..192).map(AbsoluteBN::new).collect(),
            )
            .expect("install csum-v3 journal");
        assert!(replay_dev.journal_replay_checked().failure().is_some());
        let inner = replay_dev.into_inner();
        assert!(
            inner.data[first_home..first_home + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0)
        );
        assert!(
            inner.data[second_home..second_home + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    #[test]
    fn umount_commit_propagates_device_flush_failure() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        dev.write_block(AbsoluteBN::new(10), true)
            .expect("queue metadata update");

        let error = dev
            .umount_commit()
            .expect_err("unmount commit must propagate the device error");

        assert_eq!(error, Ext4Error::io());
    }

    #[test]
    fn flush_forces_pending_journal_transaction() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let target = AbsoluteBN::new(10);
        let payload = vec![0x5a; BLOCK_SIZE];
        let sequence = dev.journal_sequence().expect("journal sequence");
        dev.write_blocks(&payload, target, 1, true)
            .expect("queue metadata update");

        dev.flush().expect("flush pending transaction");

        assert_eq!(dev.journal_sequence(), Some(sequence.wrapping_add(1)));
        let inner = dev.into_inner();
        let start = target.as_usize().expect("target offset") * BLOCK_SIZE;
        assert_eq!(&inner.data[start..start + BLOCK_SIZE], payload);
    }

    fn assert_commit_stage_fault_aborts_journal(device: MemBlockDev, stage: &str) {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, device, true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install checksummed journal");
        dev.write_block(AbsoluteBN::new(10), true)
            .expect("queue metadata update");

        let first_error = match dev.umount_commit() {
            Ok(()) => panic!("{stage} fault must fail commit"),
            Err(error) => error,
        };
        assert_eq!(
            first_error.kind(),
            crate::Ext4ErrorKind::Io,
            "{stage} must preserve the device I/O error"
        );
        let state = dev
            .abort_state
            .as_ref()
            .expect("stage fault must abort journal");
        assert_eq!(state.cause.kind(), crate::Ext4ErrorKind::Io, "{stage}");
        assert_eq!(state.persistence_error, None, "{stage}");
        assert_eq!(dev.inner._device().fua_writes, 1, "{stage}");
        assert_eq!(dev.inner._device().fail_write_call, None, "{stage}");
        assert_eq!(dev.inner._device().fail_flush_call, None, "{stage}");

        let later_error = dev
            .write_block(AbsoluteBN::new(11), true)
            .expect_err("stage fault must make abort sticky");
        assert_eq!(
            later_error.kind(),
            crate::Ext4ErrorKind::JournalAborted,
            "{stage}"
        );
    }

    #[test]
    fn commit_fault_matrix_aborts_at_every_write_and_flush_boundary() {
        let write_stages = [
            "open-superblock",
            "descriptor",
            "payload",
            "commit",
            "checkpoint",
            "close-superblock",
        ];
        for (index, stage) in write_stages.iter().enumerate() {
            assert_commit_stage_fault_aborts_journal(
                MemBlockDev::with_failing_write_call(256, index + 1),
                stage,
            );
        }

        let flush_stages = [
            "descriptor-payload-barrier",
            "commit-barrier",
            "checkpoint-barrier",
            "superblock-barrier",
        ];
        for (index, stage) in flush_stages.iter().enumerate() {
            assert_commit_stage_fault_aborts_journal(
                MemBlockDev::with_failing_flush_call(256, index + 1),
                stage,
            );
        }
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

    #[test]
    fn commit_failure_aborts_future_journal_operations() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        dev.write_block(AbsoluteBN::new(10), true)
            .expect("queue metadata update");

        let first_error = dev
            .umount_commit()
            .expect_err("first failed commit must propagate the device error");
        assert_eq!(first_error.kind(), crate::Ext4ErrorKind::Io);

        let write_error = dev
            .write_block(AbsoluteBN::new(11), true)
            .expect_err("an aborted journal must reject later metadata writes");
        assert_eq!(write_error.kind(), crate::Ext4ErrorKind::JournalAborted);

        let handle_error = dev
            .with_journal_handle(1, |_| Ok(()))
            .expect_err("an aborted journal must reject later handles");
        assert_eq!(handle_error.kind(), crate::Ext4ErrorKind::JournalAborted);

        let flush_error = dev
            .flush()
            .expect_err("an aborted journal must reject later flushes");
        assert_eq!(flush_error.kind(), crate::Ext4ErrorKind::JournalAborted);

        let unmount_error = dev
            .umount_commit()
            .expect_err("an aborted journal must not retry the transaction");
        assert_eq!(unmount_error.kind(), crate::Ext4ErrorKind::JournalAborted);

        let mode_error = dev
            .set_journal_use(false)
            .expect_err("an abort must reject journal mode changes");
        assert_eq!(mode_error.kind(), crate::Ext4ErrorKind::JournalAborted);
        let bypass_error = dev
            .write_block(AbsoluteBN::new(13), false)
            .expect_err("disabling journal use must not clear an abort");
        assert_eq!(bypass_error.kind(), crate::Ext4ErrorKind::JournalAborted);

        let reinstall_error = dev
            .set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect_err("reinstalling state must not clear an abort on the same mount object");
        assert_eq!(reinstall_error.kind(), crate::Ext4ErrorKind::JournalAborted);
    }

    #[test]
    fn commit_failure_persists_recorded_error_with_fua() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush(256), true);
        let superblock = csum_v3_superblock();
        dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
            .expect("install checksummed journal");
        dev.write_block(AbsoluteBN::new(10), true)
            .expect("queue metadata update");

        let first_error = dev
            .umount_commit()
            .expect_err("first failed commit must propagate the device error");
        assert_eq!(first_error.kind(), crate::Ext4ErrorKind::Io);

        let inner = dev.into_inner();
        assert_eq!(inner.fua_writes, 1, "abort errno must use one FUA write");
        let journal_offset = 128 * BLOCK_SIZE;
        let recorded = JournalSuperBllockS::from_disk_bytes(
            &inner.data[journal_offset..journal_offset + 1024],
        );
        assert_eq!(
            recorded.s_errno, 0xffff_fffb,
            "JBD2 stores the private generic I/O abort wire code"
        );
        assert_eq!(
            &inner.data[journal_offset + 32..journal_offset + 36],
            &[0xff, 0xff, 0xff, 0xfb]
        );
        assert_eq!(recorded.s_sequence, superblock.s_sequence);
        assert_eq!(recorded.s_start, superblock.s_first);

        let mut remount = Jbd2Dev::initial_jbd2dev(0, inner, true);
        let error = remount
            .set_journal_superblock(recorded, AbsoluteBN::new(128))
            .expect_err("a later mount must reject the recorded journal error");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);
    }

    #[test]
    fn automatic_commit_failure_aborts_the_journal() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let capacity = dev.journal_transaction_capacity().unwrap();
        let updates = vec![0x5a; capacity * BLOCK_SIZE];
        dev.write_blocks(
            &updates,
            AbsoluteBN::new(10),
            u32::try_from(capacity).unwrap(),
            true,
        )
        .expect("fill one transaction");

        let first_error = dev
            .write_block(AbsoluteBN::new(10 + capacity as u64), true)
            .expect_err("queue overflow must propagate the automatic commit failure");
        assert_eq!(first_error.kind(), crate::Ext4ErrorKind::Io);

        let unmount_error = dev
            .umount_commit()
            .expect_err("an aborted automatic commit must not be retried");
        assert_eq!(unmount_error.kind(), crate::Ext4ErrorKind::JournalAborted);
    }

    #[test]
    fn journal_cannot_be_disabled_with_pending_updates() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        dev.write_block(AbsoluteBN::new(10), true)
            .expect("queue metadata update");

        let error = dev
            .set_journal_use(false)
            .expect_err("pending journal state must not be bypassed");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
        assert!(dev.is_use_journal());

        dev.umount_commit().expect("commit pending update");
        dev.set_journal_use(false)
            .expect("disable journal after commit");
        assert!(!dev.is_use_journal());
    }

    #[test]
    fn replay_without_journal_state_latches_abort() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);

        let failure = dev
            .journal_replay_checked()
            .failure()
            .expect("replay cannot proceed without installed journal state");
        assert_eq!(failure.phase(), JournalReplayPhase::Initialize);
        assert_eq!(failure.cause().kind(), crate::Ext4ErrorKind::JournalAborted);
        let error = dev
            .set_journal_use(false)
            .expect_err("an incomplete replay must latch the journal abort");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);
    }

    #[test]
    fn journal_handle_credit_overrun_restores_queued_updates() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let target = AbsoluteBN::new(10);
        let updates = vec![0x5a; BLOCK_SIZE * 2];

        let error = dev
            .with_journal_handle(1, |dev| dev.write_blocks(&updates, target, 2, true))
            .expect_err("one credit cannot journal two distinct metadata blocks");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);

        dev.umount_commit().expect("aborted handle left no updates");
        let inner = dev.into_inner();
        for block in [target, target.checked_add(1).unwrap()] {
            let start = block.as_usize().unwrap() * BLOCK_SIZE;
            assert!(
                inner.data[start..start + BLOCK_SIZE]
                    .iter()
                    .all(|&byte| byte == 0)
            );
        }
    }

    #[test]
    fn failed_journal_handle_restores_replaced_pending_update() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let target = AbsoluteBN::new(10);
        dev.write_blocks(&vec![0x11; BLOCK_SIZE], target, 1, true)
            .expect("queue previous transaction update");

        let error = dev
            .with_journal_handle(1, |dev| {
                dev.write_blocks(&vec![0x22; BLOCK_SIZE], target, 1, true)?;
                Err::<(), _>(Ext4Error::io())
            })
            .expect_err("operation failure must abort the handle updates");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

        let mut observed = vec![0; BLOCK_SIZE];
        dev.read_blocks(&mut observed, target, 1)
            .expect("read restored pending update");
        assert_eq!(observed, vec![0x11; BLOCK_SIZE]);
        dev.umount_commit().expect("commit restored pending update");
        let inner = dev.into_inner();
        let start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[start..start + BLOCK_SIZE],
            &vec![0x11; BLOCK_SIZE]
        );
    }

    #[test]
    fn journal_handle_reserves_space_before_operation_without_auto_split() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let capacity = dev.journal_transaction_capacity().unwrap();
        let older_target = AbsoluteBN::new(10);
        let older_updates = vec![0x11; (capacity - 1) * BLOCK_SIZE];
        dev.write_blocks(
            &older_updates,
            older_target,
            u32::try_from(capacity - 1).unwrap(),
            true,
        )
        .expect("queue older running-transaction updates");
        let sequence_before = dev.journal_sequence().unwrap();

        let new_target = AbsoluteBN::new(32);
        let new_updates = vec![0x22; 2 * BLOCK_SIZE];
        let sequence_inside = dev
            .with_journal_handle(2, |dev| {
                let sequence_after_reservation = dev.journal_sequence().unwrap();
                dev.write_blocks(&new_updates, new_target, 2, true)?;
                assert_eq!(dev.journal_sequence(), Some(sequence_after_reservation));
                Ok(sequence_after_reservation)
            })
            .expect("reserved handle must keep one operation in the running transaction");

        assert_eq!(sequence_inside, sequence_before.wrapping_add(1));
        assert_eq!(dev.journal_sequence(), Some(sequence_inside));
        dev.umount_commit().expect("commit handle updates");
        assert_eq!(
            dev.journal_sequence(),
            Some(sequence_inside.wrapping_add(1))
        );
    }

    #[test]
    fn invalid_bulk_buffer_does_not_precommit_older_updates() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let capacity = dev.journal_transaction_capacity().unwrap();
        dev.write_blocks(
            &vec![0x11; (capacity - 1) * BLOCK_SIZE],
            AbsoluteBN::new(10),
            u32::try_from(capacity - 1).unwrap(),
            true,
        )
        .expect("queue older updates");
        let sequence = dev.journal_sequence();

        let error = dev
            .write_blocks(&vec![0x22; BLOCK_SIZE], AbsoluteBN::new(32), 2, true)
            .expect_err("short input cannot satisfy a two-block write");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::InvalidInput);
        assert_eq!(dev.journal_sequence(), sequence);
    }

    #[test]
    fn journal_handle_charges_one_credit_per_distinct_block() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let target = AbsoluteBN::new(10);

        dev.with_journal_handle(1, |dev| {
            dev.write_blocks(&vec![0x11; BLOCK_SIZE], target, 1, true)?;
            dev.write_blocks(&vec![0x22; BLOCK_SIZE], target, 1, true)
        })
        .expect("replacing one metadata block consumes one credit");

        let mut observed = vec![0; BLOCK_SIZE];
        dev.read_blocks(&mut observed, target, 1)
            .expect("read final queued update");
        assert_eq!(observed, vec![0x22; BLOCK_SIZE]);
        dev.umount_commit().expect("commit final queued update");
        let inner = dev.into_inner();
        let start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[start..start + BLOCK_SIZE],
            &vec![0x22; BLOCK_SIZE]
        );
    }

    #[test]
    fn nested_journal_handle_is_busy_without_poisoning_outer_handle() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let target = AbsoluteBN::new(10);

        dev.with_journal_handle(1, |dev| {
            let error = dev
                .with_journal_handle(1, |_| Ok(()))
                .expect_err("nested journal handles are not supported");
            assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
            dev.write_blocks(&vec![0x5a; BLOCK_SIZE], target, 1, true)
        })
        .expect("outer handle remains usable after nested rejection");

        dev.umount_commit().expect("commit outer handle update");
        let inner = dev.into_inner();
        let start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[start..start + BLOCK_SIZE],
            &vec![0x5a; BLOCK_SIZE]
        );
    }

    #[test]
    fn active_journal_handle_rejects_commit_and_flush_without_state_change() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let target = AbsoluteBN::new(10);
        let sequence = dev.journal_sequence();

        dev.with_journal_handle(1, |dev| {
            let error = dev
                .umount_commit()
                .expect_err("unmount cannot commit an active operation");
            assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
            assert_eq!(dev.journal_sequence(), sequence);
            let error = dev
                .flush()
                .expect_err("flush cannot commit an active operation");
            assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
            assert_eq!(dev.journal_sequence(), sequence);
            dev.write_blocks(&vec![0x5a; BLOCK_SIZE], target, 1, true)
        })
        .expect("handle remains active after rejected unmount");

        assert_eq!(dev.journal_sequence(), sequence);
        dev.umount_commit().expect("commit after handle completion");
        let inner = dev.into_inner();
        let start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[start..start + BLOCK_SIZE],
            &vec![0x5a; BLOCK_SIZE]
        );
    }
}
