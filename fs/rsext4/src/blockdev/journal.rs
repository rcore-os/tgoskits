//! JBD2-aware block device facade.

use alloc::{boxed::Box, vec::Vec};

use super::cached_device::BlockDev;
use crate::{
    bmalloc::AbsoluteBN,
    checksum::jbd2_superblock_csum32,
    disknode::Ext4Timestamp,
    error::{Ext4Error, Ext4Result},
    io::{BlockIo, WriteFlags},
    jbd2::{
        jbd2::{Jbd2CommitTimestamp, ReplayFailure, ReplayStatus},
        jbdstruct::{
            JBD2_BLOCKTYPE_SUPERBLOCK_V1, JBD2_BLOCKTYPE_SUPERBLOCK_V2,
            JBD2_FEATURE_COMPAT_CHECKSUM, JBD2_FEATURE_INCOMPAT_64BIT,
            JBD2_FEATURE_INCOMPAT_CSUM_V2, JBD2_FEATURE_INCOMPAT_CSUM_V3,
            JBD2_FEATURE_INCOMPAT_REVOKE, JBD2_MAGIC, JBD2DEVSYSTEM, Jbd2ChecksumMode,
            Jbd2RunningTransaction, Jbd2Update, JournalSuperBlock,
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
    metadata_credits: usize,
    revoke_credits_requested: usize,
    revoke_credits_remaining: usize,
    transaction_credits_at_start: usize,
    touched_metadata_blocks: Vec<AbsoluteBN>,
    queue_snapshot: Vec<Jbd2Update>,
    revoke_snapshot: Vec<AbsoluteBN>,
}

struct ActiveDirectHandle {
    credits: usize,
    before_images: Vec<Jbd2Update>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TransactionCredits {
    metadata_blocks: usize,
    revoke_records: usize,
}

impl TransactionCredits {
    pub(crate) const fn metadata(metadata_blocks: usize) -> Self {
        Self {
            metadata_blocks,
            revoke_records: 0,
        }
    }

    pub(crate) const fn metadata_with_revokes(
        metadata_blocks: usize,
        revoke_records: usize,
    ) -> Self {
        Self {
            metadata_blocks,
            revoke_records,
        }
    }

    fn total_buffer_credits(self, revoke_records_per_block: usize) -> Ext4Result<usize> {
        self.metadata_blocks
            .checked_add(self.revoke_records.div_ceil(revoke_records_per_block))
            .ok_or_else(Ext4Error::overflow)
    }

    const fn is_empty(self) -> bool {
        self.metadata_blocks == 0 && self.revoke_records == 0
    }
}

impl From<usize> for TransactionCredits {
    fn from(metadata_blocks: usize) -> Self {
        Self::metadata(metadata_blocks)
    }
}

fn checked_block_bytes(block_size: usize, count: u32) -> Ext4Result<usize> {
    usize::try_from(count)
        .map_err(|_| Ext4Error::overflow())?
        .checked_mul(block_size)
        .ok_or_else(Ext4Error::overflow)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReservedJournalHandleId(u64);

struct JournalReservation {
    id: ReservedJournalHandleId,
    credits: TransactionCredits,
    buffer_credits: usize,
}

#[derive(Debug)]
#[must_use = "a reserved journal handle must be started or explicitly freed"]
pub(crate) struct ReservedJournalHandle {
    id: ReservedJournalHandleId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransactionHandleExtension {
    Extended,
    RestartRequired,
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
    active_direct_handle: Option<ActiveDirectHandle>,
    reserved_handles: Vec<JournalReservation>,
    next_reserved_handle_id: u64,
    abort_state: Option<JournalAbortState>,
    clock: ClockCallback<B>,
}

impl<B: BlockIo> Jbd2Dev<B> {
    fn validate_journal_superblock(
        &self,
        super_block: &JournalSuperBlock,
        mapped_blocks: usize,
    ) -> Ext4Result<()> {
        let block_type = super_block.s_header.h_blocktype;
        if super_block.s_header.h_magic != JBD2_MAGIC
            || !matches!(
                block_type,
                JBD2_BLOCKTYPE_SUPERBLOCK_V1 | JBD2_BLOCKTYPE_SUPERBLOCK_V2
            )
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
        if super_block.is_v1() {
            if super_block.s_errno != 0 {
                return Err(Ext4Error::journal_aborted().with_operation("jbd2:recorded_error"));
            }
            Self::transaction_capacity(
                super_block,
                self.inner.block_size() as usize,
                mapped_blocks,
            )?;
            return Ok(());
        }
        let supported_incompat = JBD2_FEATURE_INCOMPAT_REVOKE
            | JBD2_FEATURE_INCOMPAT_64BIT
            | JBD2_FEATURE_INCOMPAT_CSUM_V2
            | JBD2_FEATURE_INCOMPAT_CSUM_V3;
        if super_block.s_feature_incompat & !supported_incompat != 0 {
            return Err(Ext4Error::unsupported().with_operation("jbd2:features"));
        }
        if super_block.s_feature_compat & !JBD2_FEATURE_COMPAT_CHECKSUM != 0
            || super_block.s_feature_ro_compat != 0
        {
            return Err(Ext4Error::unsupported().with_operation("jbd2:features"));
        }
        match super_block.checksum_mode()? {
            Jbd2ChecksumMode::CsumV2 | Jbd2ChecksumMode::CsumV3 => {
                if super_block.s_checksum != jbd2_superblock_csum32(super_block) {
                    return Err(Ext4Error::checksum().with_operation("jbd2:superblock_checksum"));
                }
            }
            Jbd2ChecksumMode::None | Jbd2ChecksumMode::CompatChecksum => {}
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
        let revoke_records_per_block = self.journal_revoke_records_per_block()?;
        let reserved_buffer_credits = self.reserved_buffer_credits()?;

        if let Some(handle) = self.active_handle.as_mut() {
            if !handle.touched_metadata_blocks.contains(&update.0) {
                if handle.touched_metadata_blocks.len() >= handle.metadata_credits {
                    return Err(Ext4Error::no_space().with_operation("jbd2:handle_credits"));
                }
                handle.touched_metadata_blocks.push(update.0);
            }
            let system = self.system.as_mut().ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:write_without_state")
            })?;
            system
                .running_transaction
                .revoked_blocks
                .retain(|block| *block != update.0);
            if let Some(existing) = system
                .running_transaction
                .updates
                .iter_mut()
                .find(|queued| queued.0 == update.0)
            {
                *existing = update;
            } else {
                system.running_transaction.updates.push(update);
            }
            return Ok(());
        }

        let needs_commit = {
            let system = self.system.as_mut().ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:write_without_state")
            })?;
            system
                .running_transaction
                .revoked_blocks
                .retain(|block| *block != update.0);
            if let Some(existing) = system
                .running_transaction
                .updates
                .iter_mut()
                .find(|queued| queued.0 == update.0)
            {
                *existing = update;
                return Ok(());
            }
            Self::running_transaction_credits(system, revoke_records_per_block)?
                .checked_add(reserved_buffer_credits)
                .ok_or_else(Ext4Error::overflow)?
                .checked_add(1)
                .ok_or_else(Ext4Error::overflow)?
                > transaction_capacity
        };

        if needs_commit {
            self.commit_pending_transaction()?;
        }

        let running_transaction_is_empty = self
            .system
            .as_ref()
            .ok_or_else(|| Ext4Error::journal_aborted().with_operation("jbd2:write_without_state"))
            .and_then(|system| {
                Self::running_transaction_credits(system, revoke_records_per_block)
            })?
            == 0;
        if running_transaction_is_empty {
            self.reserve_maximum_transaction_log_space()?;
        }

        let system = self.system.as_mut().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:write_without_state")
        })?;
        system
            .running_transaction
            .revoked_blocks
            .retain(|block| *block != update.0);
        system.running_transaction.updates.push(update);
        Ok(())
    }

    fn clone_updates(queue: &[Jbd2Update]) -> Vec<Jbd2Update> {
        queue
            .iter()
            .map(|update| Jbd2Update(update.0, update.1.to_vec().into_boxed_slice()))
            .collect()
    }

    fn visible_committed_update(
        system: &JBD2DEVSYSTEM,
        block_id: AbsoluteBN,
    ) -> Option<&Jbd2Update> {
        let mut revoked = system
            .running_transaction
            .revoked_blocks
            .contains(&block_id);
        if let Some(transaction) = &system.committing_transaction {
            if transaction.revoked_blocks.contains(&block_id) {
                revoked = true;
            }
            if !revoked
                && let Some(update) = transaction
                    .updates
                    .iter()
                    .find(|queued| queued.0 == block_id)
            {
                return Some(update);
            }
        }
        for transaction in system.checkpoint_transactions.iter().rev() {
            if transaction.revoked_blocks.contains(&block_id) {
                revoked = true;
            }
            if !revoked
                && let Some(update) = transaction
                    .updates
                    .iter()
                    .find(|queued| queued.0 == block_id)
            {
                return Some(update);
            }
        }
        None
    }

    fn running_transaction_credits(
        system: &JBD2DEVSYSTEM,
        revoke_records_per_block: usize,
    ) -> Ext4Result<usize> {
        let distinct_revokes = system
            .running_transaction
            .revoked_blocks
            .iter()
            .filter(|block| {
                !system
                    .running_transaction
                    .updates
                    .iter()
                    .any(|update| update.0 == **block)
            })
            .count();
        system
            .running_transaction
            .updates
            .len()
            .checked_add(distinct_revokes.div_ceil(revoke_records_per_block))
            .ok_or_else(Ext4Error::overflow)
    }

    fn transaction_capacity(
        superblock: &JournalSuperBlock,
        block_size: usize,
        mapped_blocks: usize,
    ) -> Ext4Result<usize> {
        let declared_blocks =
            usize::try_from(superblock.s_maxlen).map_err(|_| Ext4Error::overflow())?;
        let journal_blocks = declared_blocks.min(mapped_blocks);
        let maximum_transaction_records = journal_blocks / 3;
        let descriptor_capacity = superblock.descriptor_tag_capacity(block_size)?;
        let transaction_overhead = maximum_transaction_records
            .div_ceil(descriptor_capacity)
            .checked_add(1)
            .ok_or_else(Ext4Error::overflow)?;
        maximum_transaction_records
            .checked_sub(transaction_overhead)
            .filter(|capacity| *capacity != 0)
            .ok_or_else(|| Ext4Error::no_space().with_operation("jbd2:transaction_capacity"))
    }

    fn maximum_transaction_records(
        superblock: &JournalSuperBlock,
        mapped_blocks: usize,
    ) -> Ext4Result<usize> {
        let declared_blocks =
            usize::try_from(superblock.s_maxlen).map_err(|_| Ext4Error::overflow())?;
        let maximum = declared_blocks.min(mapped_blocks) / 3;
        if maximum == 0 {
            return Err(Ext4Error::no_space().with_operation("jbd2:transaction_capacity"));
        }
        Ok(maximum)
    }

    fn make_system(
        super_block: JournalSuperBlock,
        journal_start_block: AbsoluteBN,
    ) -> JBD2DEVSYSTEM {
        JBD2DEVSYSTEM {
            start_block: journal_start_block,
            max_len: super_block.s_maxlen,
            head: super_block.s_first,
            sequence: super_block.s_sequence,
            jbd2_super_block: super_block,
            running_transaction: Jbd2RunningTransaction {
                phase: Default::default(),
                updates: Vec::new(),
                revoked_blocks: Vec::new(),
            },
            committing_transaction: None,
            checkpoint_transactions: Vec::new(),
            used_log_records: 0,
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
            active_direct_handle: None,
            reserved_handles: Vec::new(),
            next_reserved_handle_id: 1,
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

    /// Reads home blocks without consulting cached or journal-owned images.
    pub(crate) fn read_blocks_uncached(
        &mut self,
        output: &mut [u8],
        block: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        self.inner.read_blocks(output, block, count)
    }

    /// Publishes non-journalled metadata directly at a durability boundary.
    pub(crate) fn write_blocks_durable(
        &mut self,
        input: &[u8],
        block: AbsoluteBN,
        count: u32,
    ) -> Ext4Result<()> {
        self.inner.write_blocks_with_flags(
            input,
            block,
            count,
            WriteFlags::METADATA | WriteFlags::FUA,
        )
    }

    /// Returns whether journal support is enabled.
    pub fn is_use_journal(&self) -> bool {
        self.journal_use
    }

    pub(crate) fn device_is_read_only(&self) -> bool {
        self.inner._device().capabilities().read_only
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

    fn ensure_journal_state_reinstallable(&self) -> Ext4Result<()> {
        let pending_transaction = self.system.as_ref().is_some_and(|system| {
            !system.running_transaction.updates.is_empty()
                || !system.running_transaction.revoked_blocks.is_empty()
                || system.committing_transaction.is_some()
                || !system.checkpoint_transactions.is_empty()
                || system.used_log_records != 0
        });
        if self.active_handle.is_some()
            || self.active_direct_handle.is_some()
            || !self.reserved_handles.is_empty()
            || pending_transaction
        {
            Err(Ext4Error::busy().with_operation("jbd2:reinstall_with_pending_owner"))
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
        if self.active_handle.is_some() || self.active_direct_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:commit_with_active_handle"));
        }
        if self.inner.has_unpublished_edit() {
            return Err(Ext4Error::busy().with_operation("jbd2:commit_with_unfinished_block_edit"));
        }
        let system = self.system.as_mut().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:commit_without_state")
        })?;
        if system.running_transaction.updates.is_empty()
            && system.running_transaction.revoked_blocks.is_empty()
        {
            return Ok(false);
        }
        let commit_time = Jbd2CommitTimestamp::try_from((self.clock)(self.inner._device())?)?;
        let result = system.commit_transaction_with_mapping(
            &mut self.inner,
            &self.journal_blocks,
            commit_time,
        );
        let committed = match result {
            Ok(committed) => committed,
            Err(error) => {
                self.abort_journal(error);
                return Err(error);
            }
        };
        if committed {
            // A journal update owns an immutable block copy before commit.
            // The guard above proves that no caller-owned mutable image can be
            // published while refreshing cache coherence.
            self.inner.discard_held();
        }
        Ok(committed)
    }

    fn checkpoint_transactions(&mut self, max_transactions: usize) -> Ext4Result<bool> {
        self.ensure_not_aborted("jbd2:checkpoint_after_abort")?;
        if !self.journal_use {
            return Ok(false);
        }
        if self.inner.has_unpublished_edit() {
            return Err(
                Ext4Error::busy().with_operation("jbd2:checkpoint_with_unfinished_block_edit")
            );
        }
        let Some(system) = self.system.as_mut() else {
            return Err(
                Ext4Error::journal_aborted().with_operation("jbd2:checkpoint_without_state")
            );
        };
        let result = system.checkpoint_transactions_with_mapping(
            &mut self.inner,
            &self.journal_blocks,
            max_transactions,
        );
        let checkpointed = match result {
            Ok(checkpointed) => checkpointed,
            Err(error) => {
                self.abort_journal(error);
                return Err(error);
            }
        };
        if checkpointed {
            self.inner.discard_held();
        }
        Ok(checkpointed)
    }

    #[cfg(test)]
    fn checkpoint_pending_transactions(&mut self) -> Ext4Result<bool> {
        self.checkpoint_transactions(1)
    }

    fn checkpoint_all_pending_transactions(&mut self) -> Ext4Result<bool> {
        self.checkpoint_transactions(usize::MAX)
    }

    fn checkpoint_until_log_records(&mut self, required_records: usize) -> Ext4Result<()> {
        let mut available_records = self.journal_available_log_records()?;
        let system = self.system.as_ref().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:checkpoint_without_state")
        })?;
        let mut checkpoint_count = 0usize;
        while available_records < required_records {
            let transaction = system
                .checkpoint_transactions
                .get(checkpoint_count)
                .ok_or_else(|| Ext4Error::no_space().with_operation("jbd2:log_space"))?;
            available_records = available_records
                .checked_add(transaction.log_records)
                .ok_or_else(Ext4Error::overflow)?;
            checkpoint_count = checkpoint_count
                .checked_add(1)
                .ok_or_else(Ext4Error::overflow)?;
        }
        if checkpoint_count != 0 && !self.checkpoint_transactions(checkpoint_count)? {
            return Err(Ext4Error::no_space().with_operation("jbd2:log_space"));
        }
        Ok(())
    }

    fn reserve_maximum_transaction_log_space(&mut self) -> Ext4Result<()> {
        let required_records = self.journal_maximum_transaction_records()?;
        self.checkpoint_until_log_records(required_records)
    }

    fn journal_mapped_blocks(&self) -> Ext4Result<usize> {
        let system = self.system.as_ref().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:capacity_without_state")
        })?;
        if self.journal_blocks.is_empty() {
            let available = self
                .total_blocks()
                .checked_sub(system.start_block.raw())
                .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:mapping_capacity"))?;
            usize::try_from(available).map_err(|_| Ext4Error::overflow())
        } else {
            Ok(self.journal_blocks.len())
        }
    }

    fn journal_transaction_capacity(&self) -> Ext4Result<usize> {
        self.ensure_not_aborted("jbd2:capacity_after_abort")?;
        let system = self.system.as_ref().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:capacity_without_state")
        })?;
        let mapped_blocks = self.journal_mapped_blocks()?;
        Self::transaction_capacity(
            &system.jbd2_super_block,
            self.inner.block_size() as usize,
            mapped_blocks,
        )
    }

    fn journal_revoke_records_per_block(&self) -> Ext4Result<usize> {
        self.ensure_not_aborted("jbd2:capacity_after_abort")?;
        let system = self.system.as_ref().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:capacity_without_state")
        })?;
        system
            .jbd2_super_block
            .revoke_records_per_block(self.inner.block_size() as usize)
    }

    fn reserved_buffer_credits(&self) -> Ext4Result<usize> {
        self.reserved_handles
            .iter()
            .try_fold(0usize, |total, handle| {
                total
                    .checked_add(handle.buffer_credits)
                    .ok_or_else(Ext4Error::overflow)
            })
    }

    fn reserve_journal_handle(
        &mut self,
        credits: TransactionCredits,
    ) -> Ext4Result<ReservedJournalHandle> {
        self.ensure_not_aborted("jbd2:reserve_after_abort")?;
        if !self.journal_use {
            return Err(Ext4Error::unsupported().with_operation("jbd2:reserved_handle"));
        }
        if credits.is_empty() {
            return Err(Ext4Error::invalid_input().with_operation("jbd2:reserved_credits"));
        }
        let buffer_credits =
            credits.total_buffer_credits(self.journal_revoke_records_per_block()?)?;
        let transaction_capacity = self.journal_transaction_capacity()?;
        if buffer_credits > transaction_capacity / 2 {
            return Err(Ext4Error::no_space().with_operation("jbd2:reserved_credits"));
        }
        let all_reserved = self
            .reserved_buffer_credits()?
            .checked_add(buffer_credits)
            .ok_or_else(Ext4Error::overflow)?;
        if all_reserved > transaction_capacity / 2 {
            // Linux waits for another task to release a reservation. The
            // portable core is entered exclusively, so waiting here could
            // never make progress; let the adapter retry after another owner
            // explicitly starts or frees its token.
            return Err(Ext4Error::busy().with_operation("jbd2:reserved_credits"));
        }

        let id = ReservedJournalHandleId(self.next_reserved_handle_id);
        self.next_reserved_handle_id = self
            .next_reserved_handle_id
            .checked_add(1)
            .ok_or_else(Ext4Error::overflow)?;
        self.reserved_handles.push(JournalReservation {
            id,
            credits,
            buffer_credits,
        });
        Ok(ReservedJournalHandle { id })
    }

    fn remove_journal_reservation(
        &mut self,
        reserved: ReservedJournalHandle,
    ) -> Ext4Result<JournalReservation> {
        let position = self
            .reserved_handles
            .iter()
            .position(|entry| entry.id == reserved.id)
            .ok_or_else(|| {
                Ext4Error::invalid_input().with_operation("jbd2:reserved_handle_owner")
            })?;
        Ok(self.reserved_handles.remove(position))
    }

    fn journal_maximum_transaction_records(&self) -> Ext4Result<usize> {
        self.ensure_not_aborted("jbd2:capacity_after_abort")?;
        let system = self.system.as_ref().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:capacity_without_state")
        })?;
        Self::maximum_transaction_records(&system.jbd2_super_block, self.journal_mapped_blocks()?)
    }

    fn journal_available_log_records(&self) -> Ext4Result<usize> {
        let system = self.system.as_ref().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:capacity_without_state")
        })?;
        let mapped_blocks = self.journal_mapped_blocks()?;
        let declared_blocks =
            usize::try_from(system.jbd2_super_block.s_maxlen).map_err(|_| Ext4Error::overflow())?;
        let first =
            usize::try_from(system.jbd2_super_block.s_first).map_err(|_| Ext4Error::overflow())?;
        let ring_records = declared_blocks
            .min(mapped_blocks)
            .checked_sub(first)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:ring_capacity"))?;
        let available_records = ring_records
            .checked_sub(system.used_log_records)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:log_accounting"))?;
        Ok(available_records)
    }

    /// Returns the largest metadata handle supported by the active journal.
    ///
    /// Direct-write mode has no journal ring boundary, so callers should keep
    /// their existing whole-operation transaction instead of splitting it.
    pub(crate) fn transaction_credit_limit(&self) -> Ext4Result<Option<usize>> {
        if self.journal_use {
            self.journal_transaction_capacity().map(Some)
        } else {
            Ok(None)
        }
    }

    pub(crate) fn transaction_credit_cost(&self, credits: TransactionCredits) -> Ext4Result<usize> {
        if self.journal_use {
            credits.total_buffer_credits(self.journal_revoke_records_per_block()?)
        } else {
            Ok(credits.metadata_blocks)
        }
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
        // Replay owns the authoritative home images. A fresh mount has no
        // caller-owned mutable cache edit, so invalidation must never perform
        // writeback that could overwrite replayed data.
        self.inner.discard_held();
        status
    }

    /// Enables or disables journal use when no transaction is in flight.
    pub fn set_journal_use(&mut self, use_journal: bool) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:change_mode_after_abort")?;
        if self.active_direct_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:mode_with_direct_handle"));
        }
        if !use_journal && self.journal_use {
            if self.active_handle.is_some() {
                return Err(Ext4Error::busy().with_operation("jbd2:disable_with_active_handle"));
            }
            if !self.reserved_handles.is_empty() {
                return Err(Ext4Error::busy().with_operation("jbd2:disable_with_reserved_handle"));
            }
            if self.system.as_ref().is_some_and(|system| {
                !system.running_transaction.updates.is_empty()
                    || !system.running_transaction.revoked_blocks.is_empty()
                    || system.committing_transaction.is_some()
                    || !system.checkpoint_transactions.is_empty()
            }) {
                return Err(Ext4Error::busy().with_operation("jbd2:disable_with_pending_commit"));
            }
        }
        self.journal_use = use_journal;
        Ok(())
    }

    /// Installs the journal superblock so JBD2 state can be initialized lazily.
    pub fn set_journal_superblock(
        &mut self,
        super_block: JournalSuperBlock,
        journal_start_block: AbsoluteBN,
    ) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:reinstall_after_abort")?;
        self.ensure_journal_state_reinstallable()?;
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
        super_block: JournalSuperBlock,
        journal_blocks: Vec<AbsoluteBN>,
    ) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:reinstall_after_abort")?;
        self.ensure_journal_state_reinstallable()?;
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

    /// Forces the running transaction's commit record without checkpointing it.
    ///
    /// Reads continue to observe the committed images through this journal
    /// owner. `flush` and `umount_commit` additionally write home blocks and
    /// advance the durable log tail.
    pub fn commit(&mut self) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:commit_after_abort")?;
        if !self.journal_use {
            return Ok(());
        }
        if self.active_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:commit_with_active_handle"));
        }

        self.commit_pending_transaction()?;
        Ok(())
    }

    /// Commits the running transaction for a filesystem sync operation.
    ///
    /// A successful journal commit is already durable because its commit
    /// record is published with FUA after the descriptor/data preflush. Home
    /// metadata remains owned by the checkpoint queue, matching Linux
    /// `ext4_sync_fs()` rather than the stronger `jbd2_journal_flush()` path.
    pub(crate) fn commit_for_filesystem_sync(&mut self) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:sync_after_abort")?;
        if self.active_direct_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:sync_with_direct_handle"));
        }
        if !self.journal_use {
            return self.inner.flush();
        }
        if self.active_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:sync_with_active_handle"));
        }

        if self.commit_pending_transaction()? {
            Ok(())
        } else {
            // Data writeback can exist without a new metadata transaction, so
            // a clean sync still needs a device durability boundary.
            self.inner.flush()
        }
    }

    /// Commits and checkpoints all buffered journal transactions during unmount.
    pub fn umount_commit(&mut self) -> Ext4Result<()> {
        if !self.reserved_handles.is_empty() {
            return Err(Ext4Error::busy().with_operation("jbd2:unmount_with_reserved_handle"));
        }
        self.commit()?;
        self.checkpoint_all_pending_transactions()?;
        Ok(())
    }

    /// Runs one metadata operation with a bounded number of queue credits.
    ///
    /// The handle joins this implementation's current in-memory journal queue.
    /// It prevents an automatic commit from splitting the operation and
    /// restores queued metadata images if the operation returns an error. This
    /// is not yet a complete Linux JBD2 handle: the filesystem transaction
    /// owner must also restore its caches and allocation state.
    fn with_nested_journal_handle<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        let queue_snapshot = {
            let system = self.system.as_ref().ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:nested_handle_without_state")
            })?;
            Self::clone_updates(&system.running_transaction.updates)
        };
        let revoke_snapshot = self
            .system
            .as_ref()
            .ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:nested_handle_without_state")
            })?
            .running_transaction
            .revoked_blocks
            .clone();
        let active_handle = self
            .active_handle
            .as_ref()
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:missing_active_handle"))?;
        let touched_metadata_snapshot = active_handle.touched_metadata_blocks.clone();
        let revoke_credits_remaining_snapshot = active_handle.revoke_credits_remaining;

        match operation(self) {
            Ok(value) => Ok(value),
            Err(operation_error) => {
                let system = self.system.as_mut().ok_or_else(|| {
                    Ext4Error::journal_aborted().with_operation("jbd2:nested_abort_without_state")
                })?;
                system.running_transaction.updates = queue_snapshot;
                system.running_transaction.revoked_blocks = revoke_snapshot;
                let handle = self.active_handle.as_mut().ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("jbd2:missing_active_handle")
                })?;
                handle.touched_metadata_blocks = touched_metadata_snapshot;
                handle.revoke_credits_remaining = revoke_credits_remaining_snapshot;
                // A nested filesystem owner restores its cache snapshot after
                // this return. Drop device-cache aliases dirtied by the failed
                // scope so they cannot bypass the restored journal queue.
                self.inner.discard_held();
                Err(operation_error)
            }
        }
    }

    fn run_active_journal_handle<T>(
        &mut self,
        credits: TransactionCredits,
        transaction_credits_at_start: usize,
        operation: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        let queue_snapshot = {
            let system = self.system.as_ref().ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:handle_without_state")
            })?;
            Self::clone_updates(&system.running_transaction.updates)
        };
        let revoke_snapshot = self
            .system
            .as_ref()
            .ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:handle_without_state")
            })?
            .running_transaction
            .revoked_blocks
            .clone();
        self.active_handle = Some(ActiveJournalHandle {
            metadata_credits: 0,
            revoke_credits_requested: 0,
            revoke_credits_remaining: 0,
            transaction_credits_at_start,
            touched_metadata_blocks: Vec::with_capacity(credits.metadata_blocks),
            queue_snapshot,
            revoke_snapshot,
        });
        match self.extend_transaction_credits(credits) {
            Ok(TransactionHandleExtension::Extended) => {}
            Ok(TransactionHandleExtension::RestartRequired) => {
                self.active_handle = None;
                return Err(Ext4Error::no_space().with_operation("jbd2:handle_credits"));
            }
            Err(error) => {
                self.active_handle = None;
                return Err(error);
            }
        }

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
                system.running_transaction.updates = handle.queue_snapshot;
                system.running_transaction.revoked_blocks = handle.revoke_snapshot;
                // The active cache may contain buffers dirtied after journal
                // write access was acquired. They must be discarded, never
                // flushed to home locations from an aborted handle.
                self.inner.discard_held();
                Err(operation_error)
            }
        }
    }

    fn with_journal_handle<T, C>(
        &mut self,
        credits: C,
        operation: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<T>
    where
        C: Into<TransactionCredits>,
    {
        let credits = credits.into();
        self.ensure_not_aborted("jbd2:handle_after_abort")?;
        if !self.journal_use {
            if credits.is_empty() {
                return Err(Ext4Error::invalid_input().with_operation("jbd2:handle_credits"));
            }
            if self.active_direct_handle.is_some() {
                return Err(Ext4Error::busy().with_operation("jbd2:nested_direct_handle"));
            }
            self.active_direct_handle = Some(ActiveDirectHandle {
                credits: credits.metadata_blocks,
                before_images: Vec::with_capacity(credits.metadata_blocks),
            });
            return match operation(self) {
                Ok(value) => {
                    self.active_direct_handle = None;
                    Ok(value)
                }
                Err(operation_error) => {
                    let handle = self.active_direct_handle.take().ok_or_else(|| {
                        Ext4Error::corrupted().with_operation("jbd2:missing_direct_handle")
                    })?;
                    if self.restore_direct_handle(handle).is_err() {
                        self.abort_journal(operation_error);
                    }
                    Err(operation_error)
                }
            };
        }
        if self.active_handle.is_some() {
            // Linux returns the task's current handle and increments h_ref;
            // nested callers do not reserve a second set of credits. The
            // closure lifetime supplies the matching scoped reference here.
            return self.with_nested_journal_handle(operation);
        }
        if credits.is_empty() {
            return Err(Ext4Error::invalid_input().with_operation("jbd2:handle_credits"));
        }
        let revoke_records_per_block = self.journal_revoke_records_per_block()?;
        let requested_buffer_credits = credits.total_buffer_credits(revoke_records_per_block)?;
        let transaction_capacity = self.journal_transaction_capacity()?;
        if requested_buffer_credits > transaction_capacity {
            return Err(Ext4Error::no_space().with_operation("jbd2:handle_credits"));
        }
        let reserved_buffer_credits = self.reserved_buffer_credits()?;
        if reserved_buffer_credits
            .checked_add(requested_buffer_credits)
            .ok_or_else(Ext4Error::overflow)?
            > transaction_capacity
        {
            return Err(Ext4Error::busy().with_operation("jbd2:reserved_credits"));
        }
        let (needs_commit, running_transaction_was_empty) = {
            let Some(system) = self.system.as_ref() else {
                return Err(
                    Ext4Error::journal_aborted().with_operation("jbd2:handle_without_state")
                );
            };
            let running_credits =
                Self::running_transaction_credits(system, revoke_records_per_block)?;
            let reserved = running_credits
                .checked_add(reserved_buffer_credits)
                .ok_or_else(Ext4Error::overflow)?
                .checked_add(requested_buffer_credits)
                .ok_or_else(Ext4Error::overflow)?;
            (reserved > transaction_capacity, running_credits == 0)
        };
        if needs_commit {
            self.commit_pending_transaction()?;
        }
        if needs_commit || running_transaction_was_empty {
            self.reserve_maximum_transaction_log_space()?;
        }

        let transaction_credits_at_start = Self::running_transaction_credits(
            self.system.as_ref().ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:handle_without_state")
            })?,
            revoke_records_per_block,
        )?
        .checked_add(reserved_buffer_credits)
        .ok_or_else(Ext4Error::overflow)?;

        self.run_active_journal_handle(credits, transaction_credits_at_start, operation)
    }

    /// Runs one filesystem-owned metadata transition without allowing an
    /// automatic commit to split its journal updates.
    pub(crate) fn with_transaction_handle<T>(
        &mut self,
        credits: usize,
        operation: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        self.with_journal_handle(credits, operation)
    }

    pub(crate) fn with_transaction_credits<T>(
        &mut self,
        credits: TransactionCredits,
        operation: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        self.with_journal_handle(credits, operation)
    }

    /// Commits the transaction owned by a completed scoped handle before
    /// attaching the next filesystem step to a fresh transaction.
    ///
    /// The caller must have ended the old handle scope. A detached reserved
    /// handle remains owned by this journal and can be attached to the new
    /// running transaction after the restart.
    pub(crate) fn restart_transaction<T>(
        &mut self,
        credits: TransactionCredits,
        operation: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        self.ensure_not_aborted("jbd2:restart_after_abort")?;
        if self.active_handle.is_some() || self.active_direct_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:restart_with_active_handle"));
        }
        if self.journal_use {
            // The old scoped handle has already stopped before this method is
            // entered. Request its transaction commit before attaching the
            // replacement handle, matching jbd2__journal_restart() without
            // leaking a handle or scheduler primitive across Rust closures.
            self.commit_pending_transaction()?;
        }
        self.with_journal_handle(credits, operation)
    }

    pub(crate) fn with_transaction_reservation<T>(
        &mut self,
        credits: TransactionCredits,
        reserved_credits: TransactionCredits,
        operation: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<(T, ReservedJournalHandle)> {
        if self.active_handle.is_some() || self.active_direct_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:nested_reserved_handle"));
        }
        if !self.journal_use {
            return Err(Ext4Error::unsupported().with_operation("jbd2:reserved_handle"));
        }
        let requested_buffer_credits = self.transaction_credit_cost(credits)?;
        let reserved_buffer_credits = self.transaction_credit_cost(reserved_credits)?;
        if requested_buffer_credits
            .checked_add(reserved_buffer_credits)
            .ok_or_else(Ext4Error::overflow)?
            > self.journal_transaction_capacity()?
        {
            return Err(Ext4Error::no_space().with_operation("jbd2:handle_credits"));
        }

        let reserved = self.reserve_journal_handle(reserved_credits)?;
        match self.with_journal_handle(credits, operation) {
            Ok(value) => Ok((value, reserved)),
            Err(operation_error) => {
                self.remove_journal_reservation(reserved)?;
                Err(operation_error)
            }
        }
    }

    pub(crate) fn with_reserved_transaction<T>(
        &mut self,
        reserved: ReservedJournalHandle,
        operation: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        // Linux consumes and frees a reserved handle when start-reserved
        // fails. Remove the token before any journal-state check so abort,
        // mode, or nested-owner errors cannot leave unreachable credits in
        // the ledger.
        let reservation = self.remove_journal_reservation(reserved)?;
        self.ensure_not_aborted("jbd2:start_reserved_after_abort")?;
        if !self.journal_use {
            return Err(Ext4Error::unsupported().with_operation("jbd2:reserved_handle"));
        }
        if self.active_handle.is_some() || self.active_direct_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:start_reserved_with_active_handle"));
        }

        // Consuming the token removes its detached reservation. The same
        // credits are immediately attached to the current running
        // transaction, so this path cannot commit, checkpoint, or otherwise
        // wait for log space.
        let revoke_records_per_block = self.journal_revoke_records_per_block()?;
        let transaction_credits_at_start = Self::running_transaction_credits(
            self.system.as_ref().ok_or_else(|| {
                Ext4Error::journal_aborted().with_operation("jbd2:handle_without_state")
            })?,
            revoke_records_per_block,
        )?
        .checked_add(self.reserved_buffer_credits()?)
        .ok_or_else(Ext4Error::overflow)?;
        let projected = transaction_credits_at_start
            .checked_add(reservation.buffer_credits)
            .ok_or_else(Ext4Error::overflow)?;
        if projected > self.journal_transaction_capacity()? {
            return Err(Ext4Error::corrupted().with_operation("jbd2:reserved_credit_invariant"));
        }
        self.run_active_journal_handle(reservation.credits, transaction_credits_at_start, operation)
    }

    pub(crate) fn free_reserved_transaction(
        &mut self,
        reserved: ReservedJournalHandle,
    ) -> Ext4Result<()> {
        self.remove_journal_reservation(reserved)?;
        Ok(())
    }

    /// Best-effort extension of the current metadata reservation.
    ///
    /// Linux JBD2 does not wait for log space from this operation. When the
    /// running transaction cannot accommodate the larger reservation, the
    /// filesystem owner must close its current atomic step and restart in a
    /// new transaction. This core reports that state explicitly so callers do
    /// not confuse a required restart with device space exhaustion.
    pub(crate) fn extend_transaction_credits(
        &mut self,
        additional_credits: TransactionCredits,
    ) -> Ext4Result<TransactionHandleExtension> {
        self.ensure_not_aborted("jbd2:extend_after_abort")?;
        if !self.journal_use {
            let handle = self.active_direct_handle.as_mut().ok_or_else(|| {
                Ext4Error::invalid_input().with_operation("jbd2:extend_without_handle")
            })?;
            handle.credits = handle
                .credits
                .checked_add(additional_credits.metadata_blocks)
                .ok_or_else(Ext4Error::overflow)?;
            return Ok(TransactionHandleExtension::Extended);
        }

        let handle = self.active_handle.as_ref().ok_or_else(|| {
            Ext4Error::invalid_input().with_operation("jbd2:extend_without_handle")
        })?;
        let Some(extended_metadata_credits) = handle
            .metadata_credits
            .checked_add(additional_credits.metadata_blocks)
        else {
            return Ok(TransactionHandleExtension::RestartRequired);
        };
        let Some(extended_revoke_credits) = handle
            .revoke_credits_requested
            .checked_add(additional_credits.revoke_records)
        else {
            return Ok(TransactionHandleExtension::RestartRequired);
        };
        let revoke_records_per_block = self.journal_revoke_records_per_block()?;
        let Some(handle_buffer_credits) = extended_metadata_credits
            .checked_add(extended_revoke_credits.div_ceil(revoke_records_per_block))
        else {
            return Ok(TransactionHandleExtension::RestartRequired);
        };
        let Some(reserved_credits) = handle
            .transaction_credits_at_start
            .checked_add(handle_buffer_credits)
        else {
            return Ok(TransactionHandleExtension::RestartRequired);
        };
        // Extending an attached handle is bounded by the transaction size,
        // not by currently free ring records. Linux JBD2 deliberately does
        // not wait for log space here; start/restart owns that concern.
        if reserved_credits > self.journal_transaction_capacity()? {
            return Ok(TransactionHandleExtension::RestartRequired);
        }

        let handle = self
            .active_handle
            .as_mut()
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:missing_active_handle"))?;
        handle.metadata_credits = extended_metadata_credits;
        handle.revoke_credits_requested = extended_revoke_credits;
        handle.revoke_credits_remaining = handle
            .revoke_credits_remaining
            .checked_add(additional_credits.revoke_records)
            .ok_or_else(Ext4Error::overflow)?;
        Ok(TransactionHandleExtension::Extended)
    }

    /// Extends the current scoped transaction when one exists.
    ///
    /// Best-effort metadata normalization leaves a valid on-disk shape
    /// unchanged when a low-level caller has no transaction owner.
    pub(crate) fn extend_active_transaction_credits(
        &mut self,
        additional_credits: TransactionCredits,
    ) -> Ext4Result<Option<TransactionHandleExtension>> {
        if self.active_handle.is_none() && self.active_direct_handle.is_none() {
            return Ok(None);
        }
        self.extend_transaction_credits(additional_credits)
            .map(Some)
    }

    fn capture_direct_preimage(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        let Some(handle) = self.active_direct_handle.as_ref() else {
            return Ok(());
        };
        if handle
            .before_images
            .iter()
            .any(|before| before.0 == block_id)
        {
            return Ok(());
        }
        if handle.before_images.len() >= handle.credits {
            return Err(Ext4Error::no_space().with_operation("jbd2:handle_credits"));
        }

        let before = if let Some(held) = self.inner.clean_buffer_for_block(block_id) {
            held.to_vec()
        } else {
            let mut before = alloc::vec![0; self.inner.block_size() as usize];
            self.inner.read_blocks(&mut before, block_id, 1)?;
            before
        };
        self.active_direct_handle
            .as_mut()
            .ok_or_else(|| Ext4Error::corrupted().with_operation("jbd2:missing_direct_handle"))?
            .before_images
            .push(Jbd2Update(block_id, before.into_boxed_slice()));
        Ok(())
    }

    fn restore_direct_handle(&mut self, handle: ActiveDirectHandle) -> Ext4Result<()> {
        self.inner.discard_held();
        let mut first_error = None;
        for before in handle.before_images.into_iter().rev() {
            if let Err(error) = self.inner.write_blocks(&before.1, before.0, 1)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        self.inner.discard_held();
        first_error.map_or(Ok(()), Err)
    }

    /// Writes the current internal block buffer.
    pub(crate) fn write_block(
        &mut self,
        block_id: AbsoluteBN,
        is_metadata: bool,
    ) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:write_after_abort")?;
        if !self.journal_use || !is_metadata {
            return self.inner.write_block(block_id);
        }

        let new_buf = self
            .inner
            .buffer_for_block(block_id)?
            .to_vec()
            .into_boxed_slice();
        let updates = Jbd2Update(block_id, new_buf);
        let transaction_capacity = match self.journal_transaction_capacity() {
            Ok(capacity) => capacity,
            Err(error) => {
                self.inner.discard_held();
                return Err(error);
            }
        };
        if let Err(error) = self.enqueue_journal_update(updates, transaction_capacity) {
            self.inner.discard_held();
            return Err(error);
        }
        // The journal queue now owns the modified image. Keeping the same
        // buffer dirty in the generic device cache could write it to the home
        // block before commit.
        self.inner.publish_journaled_block(block_id);
        Ok(())
    }

    /// Drops an uncommitted update for a newly allocated metadata block.
    ///
    /// This is only valid while rolling back a block that has not become
    /// reachable from durable filesystem metadata. Published blocks require a
    /// revoke-aware transaction instead of queue removal.
    pub(crate) fn forget_unpublished_metadata(&mut self, block_id: AbsoluteBN) {
        if let Some(system) = self.system.as_mut() {
            system
                .running_transaction
                .updates
                .retain(|update| update.0 != block_id);
        }
        self.inner.discard_block(block_id);
    }

    /// Records a revoke after published metadata is detached.
    ///
    /// A running handle keeps earlier committed transactions available for
    /// checkpoint while the revoke protects allocator reuse. Calls without a
    /// handle first close the existing boundary so detachment cannot be split
    /// from an unrelated running transaction.
    pub(crate) fn forget_detached_metadata(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:revoke_after_abort")?;
        let needs_boundary = self.journal_use
            && self.active_handle.is_none()
            && self.system.as_ref().is_some_and(|system| {
                !system.running_transaction.updates.is_empty()
                    || !system.running_transaction.revoked_blocks.is_empty()
                    || system.committing_transaction.is_some()
                    || !system.checkpoint_transactions.is_empty()
            });
        if needs_boundary {
            self.commit_pending_transaction()?;
            self.checkpoint_all_pending_transactions()?;
        }
        if self.journal_use && self.active_handle.is_none() {
            self.reserve_maximum_transaction_log_space()?;
        }
        let needs_revoke_credit = self.journal_use
            && self.active_handle.is_some()
            && self.system.as_ref().is_some_and(|system| {
                !system
                    .running_transaction
                    .revoked_blocks
                    .contains(&block_id)
            });
        if needs_revoke_credit {
            let handle = self.active_handle.as_mut().ok_or_else(|| {
                Ext4Error::corrupted().with_operation("jbd2:missing_active_handle")
            })?;
            if handle.revoke_credits_remaining == 0 {
                return Err(Ext4Error::no_space().with_operation("jbd2:revoke_credits"));
            }
            handle.revoke_credits_remaining -= 1;
        }
        if let Some(system) = self.system.as_mut() {
            system
                .running_transaction
                .updates
                .retain(|update| update.0 != block_id);
            if self.journal_use
                && !system
                    .running_transaction
                    .revoked_blocks
                    .contains(&block_id)
            {
                system.running_transaction.revoked_blocks.push(block_id);
            }
        }
        self.inner.discard_block(block_id);
        Ok(())
    }

    /// Reads one block through the cached inner device.
    pub fn read_block(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
        if self.journal_use
            && let Some(system) = self.system.as_ref()
            && let Some(update) = system
                .running_transaction
                .updates
                .iter()
                .find(|queued| queued.0 == block_id)
        {
            self.inner.cache_clean_block(block_id, &update.1[..])?;
            return Ok(());
        }
        if self.journal_use
            && let Some(update) = self
                .system
                .as_ref()
                .and_then(|system| Self::visible_committed_update(system, block_id))
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

    /// Returns the cached block buffer mutably for low-level state tests.
    #[cfg(test)]
    pub(crate) fn buffer_mut(&mut self) -> &mut [u8] {
        self.inner.buffer_mut()
    }

    /// Updates one block and transfers the finished image to its durability owner.
    ///
    /// The mutable cache image cannot escape this closure. An operation or
    /// write failure discards it; only a successful closure can publish the
    /// image either through JBD2 metadata ownership or the direct device path.
    pub(crate) fn update_block<T>(
        &mut self,
        block_id: AbsoluteBN,
        is_metadata: bool,
        operation: impl FnOnce(&mut [u8]) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        self.read_block(block_id)?;
        // Like ext4_journal_get_write_access(), direct mode acquires the
        // rollback owner before the buffer becomes mutable. Capturing after
        // `buffer_mut()` would either observe the new image or need an
        // incoherent home-block reread.
        if !self.journal_use
            && is_metadata
            && let Err(error) = self.capture_direct_preimage(block_id)
        {
            self.inner.discard_held();
            return Err(error);
        }
        let value = match operation(self.inner.buffer_mut()) {
            Ok(value) => value,
            Err(error) => {
                self.inner.discard_held();
                return Err(error);
            }
        };
        if let Err(error) = self.write_block(block_id, is_metadata) {
            self.inner.discard_held();
            return Err(error);
        }
        Ok(value)
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
        let required = checked_block_bytes(block_size, count)?;
        if buf.len() < required {
            return Err(Ext4Error::buffer_too_small(buf.len(), required));
        }

        self.inner.read_blocks(buf, block_id, count)?;

        let Some(system) = self.system.as_ref() else {
            return Ok(());
        };
        for i in 0..count {
            let bid = block_id.checked_add(i)?;
            let update = system
                .running_transaction
                .updates
                .iter()
                .find(|queued| queued.0 == bid)
                .or_else(|| Self::visible_committed_update(system, bid));
            if let Some(update) = update {
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
            if is_metadata {
                for offset in 0..count {
                    self.capture_direct_preimage(block_id.checked_add(offset)?)?;
                }
            }
            return self.inner.write_blocks(buf, block_id, count);
        }

        let block_size = self.inner.block_size() as usize;
        let required = checked_block_bytes(block_size, count)?;
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
        if self.active_direct_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:flush_with_direct_handle"));
        }
        let checkpointed = if self.journal_use {
            if self.active_handle.is_some() {
                return Err(Ext4Error::busy().with_operation("jbd2:flush_with_active_handle"));
            }
            self.commit_pending_transaction()?;
            self.checkpoint_all_pending_transactions()?
        } else {
            false
        };

        if checkpointed {
            // Checkpointing flushes the home blocks before publishing the new
            // journal tail with FUA. As in Linux jbd2_journal_flush(), that
            // publication is the final durability boundary; another device
            // flush here would be redundant.
            Ok(())
        } else {
            self.inner.flush()
        }
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
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
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
        let recorded =
            JournalSuperBlock::from_disk_bytes(&inner.data[journal_offset..journal_offset + 1024]);
        assert_eq!(
            recorded.s_errno, 0,
            "a failed FUA write must not claim the abort was recorded"
        );
    }

    #[test]
    fn journal_superblock_must_match_filesystem_block_size() {
        let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
        let superblock = JournalSuperBlock {
            s_blocksize: 1024,
            ..Default::default()
        };

        let error = dev
            .validate_journal_superblock(&superblock, superblock.s_maxlen as usize)
            .expect_err("journal and filesystem block sizes must match");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::BadSuperblock);
    }

    #[test]
    fn journal_v1_ignores_v2_extension_fields() {
        let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
        let superblock = JournalSuperBlock {
            s_header: crate::jbd2::jbdstruct::JournalHeaderS {
                h_blocktype: JBD2_BLOCKTYPE_SUPERBLOCK_V1,
                ..Default::default()
            },
            s_maxlen: 16,
            s_feature_compat: u32::MAX,
            s_feature_incompat: u32::MAX,
            s_feature_ro_compat: u32::MAX,
            s_checksum_type: u8::MAX,
            s_checksum: u32::MAX,
            ..Default::default()
        };

        dev.validate_journal_superblock(&superblock, 16)
            .expect("Linux ignores version-2 extension fields on a v1 journal");
    }

    #[test]
    fn journal_v1_commits_without_interpreting_or_rewriting_v2_tail() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        let superblock = JournalSuperBlock {
            s_header: crate::jbd2::jbdstruct::JournalHeaderS {
                h_blocktype: JBD2_BLOCKTYPE_SUPERBLOCK_V1,
                ..Default::default()
            },
            s_maxlen: 16,
            s_feature_compat: u32::MAX,
            s_feature_incompat: u32::MAX,
            s_feature_ro_compat: u32::MAX,
            s_checksum_type: u8::MAX,
            s_checksum: 0xa5a5_5a5a,
            ..Default::default()
        };
        dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
            .unwrap();

        let target = AbsoluteBN::new(10);
        let payload = vec![0x5a; BLOCK_SIZE];
        dev.write_blocks(&payload, target, 1, true).unwrap();
        dev.umount_commit().unwrap();

        let inner = dev.into_inner();
        let home_offset = target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(&inner.data[home_offset..home_offset + BLOCK_SIZE], &payload);
        let journal_offset = 128 * BLOCK_SIZE;
        let persisted = JournalSuperBlock::decode_checked(
            &inner.data[journal_offset..journal_offset + BLOCK_SIZE],
        )
        .unwrap();
        assert!(persisted.is_v1());
        assert_eq!(persisted.s_sequence, 2);
        assert_eq!(persisted.s_start, 0);
        assert_eq!(persisted.s_feature_incompat, u32::MAX);
        assert_eq!(persisted.s_checksum_type, u8::MAX);
        assert_eq!(persisted.s_checksum, 0xa5a5_5a5a);
    }

    #[test]
    fn journal_superblock_checksum_is_verified_before_use() {
        let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
        let mut superblock = JournalSuperBlock::default();
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
    fn journal_superblock_requires_block_checksum_feature_and_crc32c_together() {
        let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
        let mut missing_type = JournalSuperBlock::default();
        missing_type.s_feature_incompat |= JBD2_FEATURE_INCOMPAT_CSUM_V3;
        let error = dev
            .validate_journal_superblock(&missing_type, missing_type.s_maxlen as usize)
            .expect_err("csum-v3 requires CRC32C journal superblock checksums");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Unsupported);

        let mut missing_feature = JournalSuperBlock {
            s_checksum_type: JBD2_CRC32C_CHKSUM,
            ..Default::default()
        };
        crate::checksum::jbd2_update_superblock_checksum(&mut missing_feature);
        let error = dev
            .validate_journal_superblock(&missing_feature, missing_feature.s_maxlen as usize)
            .expect_err("CRC32C journal superblock checksums require csum-v3 support");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Unsupported);
    }

    #[test]
    fn journal_superblock_accepts_linux_csum_v2_mode() {
        let dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(32), true);
        let mut superblock = JournalSuperBlock {
            s_maxlen: 16,
            s_feature_incompat: JBD2_FEATURE_INCOMPAT_CSUM_V2,
            s_checksum_type: JBD2_CRC32C_CHKSUM,
            s_uuid: [0x3c; JBD2_UUID_SIZE],
            ..Default::default()
        };
        crate::checksum::jbd2_update_superblock_checksum(&mut superblock);

        dev.validate_journal_superblock(&superblock, 16)
            .expect("Linux CSUM_V2 journal must be accepted");
    }

    #[test]
    fn transaction_capacity_reserves_linux_third_of_log_and_bookkeeping() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        let superblock = csum_v3_superblock();
        dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
            .expect("install csum-v3 journal");

        assert_eq!(dev.journal_maximum_transaction_records().unwrap(), 21);
        assert_eq!(dev.journal_transaction_capacity().unwrap(), 19);
        let large_ring = JournalSuperBlock {
            s_maxlen: 4096,
            ..superblock
        };
        assert_eq!(
            Jbd2Dev::<MemBlockDev>::transaction_capacity(&large_ring, 1024, 4096).unwrap(),
            1341
        );

        let small = small_journal_superblock();
        assert_eq!(
            Jbd2Dev::<MemBlockDev>::transaction_capacity(&small, BLOCK_SIZE, 16).unwrap(),
            3
        );
    }

    #[test]
    fn reserved_handle_is_limited_to_half_the_user_transaction_capacity() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install csum-v3 journal");
        assert_eq!(dev.journal_transaction_capacity().unwrap(), 19);
        let mut operation_started = false;

        let error = dev
            .with_transaction_reservation(
                TransactionCredits::metadata(1),
                TransactionCredits::metadata(10),
                |_| {
                    operation_started = true;
                    Ok(())
                },
            )
            .expect_err("Linux limits journal-wide reservations to half the user capacity");

        assert!(!operation_started, "the rejected operation must not start");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
        assert_eq!(
            error.context(),
            Some(crate::ErrorContext::Operation {
                op: "jbd2:reserved_credits"
            })
        );
    }

    #[test]
    fn bulk_block_byte_count_reports_overflow_without_panicking() {
        let result = std::panic::catch_unwind(|| checked_block_bytes(usize::MAX, 2));
        let bytes = result.expect("checked byte-count arithmetic must not panic");
        assert_eq!(bytes.unwrap_err().kind(), crate::Ext4ErrorKind::Overflow);
    }

    #[test]
    fn reserved_handle_attaches_without_switching_the_running_transaction() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let first = AbsoluteBN::new(10);
        let second = AbsoluteBN::new(11);

        let ((), reserved) = dev
            .with_transaction_reservation(
                TransactionCredits::metadata(1),
                TransactionCredits::metadata(1),
                |dev| dev.write_blocks(&vec![0x31; BLOCK_SIZE], first, 1, true),
            )
            .expect("parent handle and reservation fit the transaction");
        let sequence = dev.journal_sequence().expect("journal sequence");

        dev.with_reserved_transaction(reserved, |dev| {
            assert_eq!(
                dev.journal_sequence(),
                Some(sequence),
                "starting a reserved handle must not commit or switch transactions"
            );
            dev.write_blocks(&vec![0x42; BLOCK_SIZE], second, 1, true)
        })
        .expect("attach reserved handle");
        assert_eq!(dev.journal_sequence(), Some(sequence));

        dev.commit().expect("commit shared transaction");
        assert_eq!(
            dev.system.as_ref().unwrap().checkpoint_transactions[0]
                .updates
                .len(),
            2
        );
    }

    #[test]
    fn detached_reservation_is_counted_before_an_ordinary_handle_starts() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");

        let ((), reserved) = dev
            .with_transaction_reservation(
                TransactionCredits::metadata(1),
                TransactionCredits::metadata(1),
                |dev| dev.write_blocks(&vec![0x51; BLOCK_SIZE], AbsoluteBN::new(10), 1, true),
            )
            .expect("create detached reservation");
        let sequence = dev.journal_sequence().expect("journal sequence");

        dev.with_transaction_handle(2, |dev| {
            assert_ne!(
                dev.journal_sequence(),
                Some(sequence),
                "the earlier transaction must commit before credits can overlap the reservation"
            );
            Ok(())
        })
        .expect("ordinary handle starts after capacity is reclaimed");
        dev.free_reserved_transaction(reserved)
            .expect("release unused reservation");
    }

    #[test]
    fn failed_parent_handle_releases_its_reserved_credits() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install csum-v3 journal");

        let error = dev
            .with_transaction_reservation(
                TransactionCredits::metadata(1),
                TransactionCredits::metadata(9),
                |_| Err::<(), _>(Ext4Error::io().with_operation("test:reserved_parent_abort")),
            )
            .expect_err("parent failure must not publish its reserved handle");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);
        assert!(dev.reserved_handles.is_empty());

        let ((), reserved) = dev
            .with_transaction_reservation(
                TransactionCredits::metadata(1),
                TransactionCredits::metadata(9),
                |_| Ok(()),
            )
            .expect("the full half-transaction reservation must be available again");
        dev.free_reserved_transaction(reserved)
            .expect("release replacement reservation");
        assert!(dev.reserved_handles.is_empty());
    }

    #[test]
    fn journal_wide_reserved_credits_do_not_exceed_half_the_capacity() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install csum-v3 journal");

        let ((), first) = dev
            .with_transaction_reservation(
                TransactionCredits::metadata(1),
                TransactionCredits::metadata(5),
                |_| Ok(()),
            )
            .expect("first reservation fits");
        let mut operation_started = false;
        let error = dev
            .with_transaction_reservation(
                TransactionCredits::metadata(1),
                TransactionCredits::metadata(5),
                |_| {
                    operation_started = true;
                    Ok(())
                },
            )
            .expect_err("the aggregate reservation exceeds half the capacity");
        assert!(!operation_started);
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
        dev.free_reserved_transaction(first)
            .expect("release first reservation");
    }

    #[test]
    fn failed_start_reserved_consumes_the_detached_token() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install csum-v3 journal");
        let ((), reserved) = dev
            .with_transaction_reservation(
                TransactionCredits::metadata(1),
                TransactionCredits::metadata(1),
                |_| Ok(()),
            )
            .expect("create detached reservation");
        dev.abort_journal(Ext4Error::io().with_operation("test:abort_before_start_reserved"));

        let error = dev
            .with_reserved_transaction(reserved, |_| Ok(()))
            .expect_err("start-reserved must report the sticky abort");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::JournalAborted);
        assert!(
            dev.reserved_handles.is_empty(),
            "a failed start-reserved consumes and frees the token"
        );
    }

    #[test]
    fn detached_reserved_handle_must_be_resolved_before_unmount() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install csum-v3 journal");
        let ((), reserved) = dev
            .with_transaction_reservation(
                TransactionCredits::metadata(1),
                TransactionCredits::metadata(1),
                |_| Ok(()),
            )
            .expect("create detached reservation");

        let error = dev
            .umount_commit()
            .expect_err("unmount cannot discard a live reservation");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
        assert_eq!(
            error.context(),
            Some(crate::ErrorContext::Operation {
                op: "jbd2:unmount_with_reserved_handle"
            })
        );
        dev.free_reserved_transaction(reserved)
            .expect("release reservation");
        dev.umount_commit().expect("unmount after explicit release");
    }

    #[test]
    fn revoke_records_consume_descriptor_credits_instead_of_metadata_credits() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(4096), true);
        let superblock = csum_v3_superblock();
        dev.set_journal_superblock(superblock, AbsoluteBN::new(2048))
            .expect("install csum-v3 journal");
        let revoke_records_per_block = (BLOCK_SIZE
            - core::mem::size_of::<Jbd2JournalRevokeHeadS>()
            - core::mem::size_of::<u32>())
            / core::mem::size_of::<u64>();
        let metadata_target = AbsoluteBN::new(1536);

        dev.with_transaction_credits(
            TransactionCredits::metadata_with_revokes(1, revoke_records_per_block),
            |dev| {
                for index in 0..revoke_records_per_block {
                    dev.forget_detached_metadata(AbsoluteBN::new(100 + index as u64))?;
                }
                dev.write_blocks(&vec![0x5a; BLOCK_SIZE], metadata_target, 1, true)
            },
        )
        .expect("one full revoke descriptor and one metadata update must fit two credits");

        dev.commit().expect("commit revoke-credit transaction");
        let system = dev.system.as_ref().expect("journal state");
        assert_eq!(system.used_log_records, 4);
        assert_eq!(system.checkpoint_transactions.len(), 1);
    }

    #[test]
    fn revoke_beyond_handle_request_fails_and_restores_the_transaction() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install csum-v3 journal");

        let error = dev
            .with_transaction_credits(TransactionCredits::metadata_with_revokes(0, 1), |dev| {
                dev.forget_detached_metadata(AbsoluteBN::new(10))?;
                dev.forget_detached_metadata(AbsoluteBN::new(11))
            })
            .expect_err("a handle must not consume an unrequested revoke record");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
        assert_eq!(
            error.context(),
            Some(crate::ErrorContext::Operation {
                op: "jbd2:revoke_credits"
            })
        );
        assert!(
            dev.system
                .as_ref()
                .unwrap()
                .running_transaction
                .revoked_blocks
                .is_empty(),
            "the failed handle must restore its revoke-table snapshot"
        );
    }

    #[test]
    fn revoke_extension_charges_only_a_new_descriptor_boundary() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let revoke_records_per_block = dev
            .journal_revoke_records_per_block()
            .expect("revoke capacity");
        assert_eq!(dev.journal_transaction_capacity().unwrap(), 3);

        dev.with_transaction_credits(
            TransactionCredits::metadata_with_revokes(2, revoke_records_per_block - 1),
            |dev| {
                assert_eq!(
                    dev.extend_transaction_credits(TransactionCredits::metadata_with_revokes(
                        0, 1
                    ),)?,
                    TransactionHandleExtension::Extended,
                    "filling the existing revoke descriptor costs no new buffer credit"
                );
                assert_eq!(
                    dev.extend_transaction_credits(TransactionCredits::metadata_with_revokes(
                        0, 1
                    ),)?,
                    TransactionHandleExtension::RestartRequired,
                    "crossing the descriptor boundary exceeds the fixed transaction capacity"
                );
                let handle = dev.active_handle.as_ref().expect("active handle");
                assert_eq!(handle.revoke_credits_requested, revoke_records_per_block);
                assert_eq!(handle.revoke_credits_remaining, revoke_records_per_block);
                Ok(())
            },
        )
        .expect("the original revoke reservation remains valid");
    }

    #[test]
    fn one_transaction_can_span_multiple_descriptor_blocks() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(2048), true);
        let mut superblock = JournalSuperBlock {
            s_maxlen: 1024,
            ..csum_v3_superblock()
        };
        crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
        dev.set_journal_superblock(superblock, AbsoluteBN::new(512))
            .expect("install journal with room for multiple descriptors");

        let target = AbsoluteBN::new(10);
        let payload_count = 255usize;
        dev.with_transaction_handle(payload_count, |device| {
            for index in 0..payload_count {
                let payload = vec![(index + 1) as u8; BLOCK_SIZE];
                device.write_blocks(
                    &payload,
                    target.checked_add(u32::try_from(index).unwrap()).unwrap(),
                    1,
                    true,
                )?;
            }
            Ok(())
        })
        .expect("one handle may exceed one descriptor's tag capacity");
        dev.umount_commit()
            .expect("commit multi-descriptor transaction");

        assert_eq!(dev.journal_sequence(), Some(2));
        let mut inner = dev.into_inner();
        let second_descriptor = &inner.data[767 * BLOCK_SIZE..768 * BLOCK_SIZE];
        let header = JournalHeaderS::from_disk_bytes(second_descriptor);
        assert_eq!(
            header.h_blocktype,
            crate::jbd2::jbdstruct::JBD2_BLOCKTYPE_DESCRIPTOR
        );
        assert_eq!(header.h_sequence, 1);
        let second_tag_offset = JBD2_DESCRIPTOR_HEADER_SIZE + JBD2_TAG3_SIZE + JBD2_UUID_SIZE;
        let second_tag = JournalBlockTag3S::from_disk_bytes(
            &second_descriptor[second_tag_offset..second_tag_offset + JBD2_TAG3_SIZE],
        );
        assert_ne!(
            second_tag.t_flags & u32::from(crate::jbd2::jbdstruct::JBD2_FLAG_LAST_TAG),
            0
        );

        for index in 0..payload_count {
            let block = target.checked_add(u32::try_from(index).unwrap()).unwrap();
            let start = block.as_usize().unwrap() * BLOCK_SIZE;
            inner.data[start..start + BLOCK_SIZE].fill(0);
        }
        let mut replay_superblock = superblock;
        replay_superblock.s_start = replay_superblock.s_first;
        replay_superblock.s_sequence = 1;
        crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
        replay_superblock.to_disk_bytes(&mut inner.data[512 * BLOCK_SIZE..][..1024]);

        let mut replay = Jbd2Dev::initial_jbd2dev(0, inner, true);
        replay
            .set_journal_superblock_with_mapping(
                replay_superblock,
                (512..1536).map(AbsoluteBN::new).collect(),
            )
            .expect("install committed multi-descriptor journal");
        assert_eq!(replay.journal_replay_checked(), ReplayStatus::Complete);
        let inner = replay.into_inner();
        for index in 0..payload_count {
            let block = target.checked_add(u32::try_from(index).unwrap()).unwrap();
            let start = block.as_usize().unwrap() * BLOCK_SIZE;
            assert!(
                inner.data[start..start + BLOCK_SIZE]
                    .iter()
                    .all(|byte| *byte == (index + 1) as u8)
            );
        }
    }

    #[test]
    fn second_descriptor_write_failure_never_checkpoints_the_first_chunk() {
        let mut inner = MemBlockDev::new(2048);
        inner.fail_next_write_at_block(AbsoluteBN::new(767));
        let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        let mut superblock = JournalSuperBlock {
            s_maxlen: 1024,
            ..csum_v3_superblock()
        };
        crate::checksum::jbd2_update_superblock_checksum(&mut superblock);
        dev.set_journal_superblock(superblock, AbsoluteBN::new(512))
            .expect("install journal with room for multiple descriptors");

        let target = AbsoluteBN::new(10);
        let payload_count = 255usize;
        dev.with_transaction_handle(payload_count, |device| {
            for index in 0..payload_count {
                let payload = vec![(index + 1) as u8; BLOCK_SIZE];
                device.write_blocks(
                    &payload,
                    target.checked_add(u32::try_from(index).unwrap()).unwrap(),
                    1,
                    true,
                )?;
            }
            Ok(())
        })
        .expect("queue one multi-descriptor transaction");

        let error = dev
            .umount_commit()
            .expect_err("second descriptor fault must abort before commit");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);
        let later = dev
            .write_block(AbsoluteBN::new(300), true)
            .expect_err("descriptor write fault must latch journal abort");
        assert_eq!(later.kind(), crate::Ext4ErrorKind::JournalAborted);

        let inner = dev.into_inner();
        for index in 0..payload_count {
            let block = target.checked_add(u32::try_from(index).unwrap()).unwrap();
            let start = block.as_usize().unwrap() * BLOCK_SIZE;
            assert!(
                inner.data[start..start + BLOCK_SIZE]
                    .iter()
                    .all(|byte| *byte == 0),
                "uncommitted descriptor prefix must not reach home block {block:?}"
            );
        }
    }

    #[test]
    fn journal_install_rejects_ring_without_payload_capacity() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        let too_small = JournalSuperBlock {
            s_maxlen: 3,
            s_first: 1,
            ..JournalSuperBlock::default()
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
    fn commit_record_uses_the_injected_filesystem_clock() {
        let mut dev = Jbd2Dev::with_clock(0, MemBlockDev::new(256), FixedCommitClock, true);
        let superblock = csum_v3_superblock();
        dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
            .expect("install csum-v3 journal");
        dev.write_blocks(&vec![0xa5; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
            .expect("queue metadata");

        dev.umount_commit().expect("commit metadata");

        let inner = dev.into_inner();
        let commit = CommitHeader::from_disk_bytes(&inner.data[131 * BLOCK_SIZE..132 * BLOCK_SIZE]);
        assert_eq!(commit.h_commit_sec, 1_723_456_789);
        assert_eq!(commit.h_commit_nsec, 123_456_789);
    }

    #[test]
    fn empty_commit_does_not_read_the_filesystem_clock() {
        let mut dev = Jbd2Dev::with_clock(0, MemBlockDev::new(256), FailingCommitClock, true);
        let superblock = csum_v3_superblock();
        dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
            .expect("install csum-v3 journal");

        dev.commit().expect("empty commit must not need time");
    }

    #[test]
    fn commit_rejects_invalid_filesystem_time_before_switching_transaction_owner() {
        for timestamp in [
            Ext4Timestamp { sec: -1, nsec: 0 },
            Ext4Timestamp {
                sec: 1,
                nsec: Ext4Timestamp::MAX_NSEC + 1,
            },
        ] {
            let mut dev = Jbd2Dev::with_clock(
                0,
                MemBlockDev::new(256),
                InvalidCommitClock(timestamp),
                true,
            );
            let superblock = csum_v3_superblock();
            dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
                .expect("install csum-v3 journal");
            dev.write_blocks(&vec![0xa5; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
                .expect("queue metadata");

            let error = dev
                .commit()
                .expect_err("invalid clock value must not reach a commit record");

            assert_eq!(error.kind(), crate::Ext4ErrorKind::InvalidInput);
            assert!(dev.journal_abort_cause().is_none());
            let system = dev.system.as_ref().expect("journal state");
            assert_eq!(system.running_transaction.updates.len(), 1);
            assert!(system.committing_transaction.is_none());
        }
    }

    #[test]
    fn csum_v2_commit_emits_linux_tag_padding_and_block_checksums() {
        let (inner, superblock, target) = committed_csum_v2_fixture();
        let descriptor = &inner.data[129 * BLOCK_SIZE..130 * BLOCK_SIZE];
        let tag = JournalBlockTagS::from_disk_bytes(
            &descriptor[JBD2_DESCRIPTOR_HEADER_SIZE..JBD2_DESCRIPTOR_HEADER_SIZE + 8],
        );
        assert_eq!(tag.t_blocknr, target.raw() as u32);
        assert_eq!(
            tag.t_checksum,
            reference_jbd2_tag_checksum(
                &superblock.s_uuid,
                1,
                &inner.data[130 * BLOCK_SIZE..131 * BLOCK_SIZE],
            ) as u16
        );
        assert_eq!(
            &descriptor[JBD2_DESCRIPTOR_HEADER_SIZE + 8..JBD2_DESCRIPTOR_HEADER_SIZE + 10],
            &[0, 0],
            "Linux reserves two zero bytes in a 32-bit CSUM_V2 tag"
        );
        assert_eq!(
            &descriptor[JBD2_DESCRIPTOR_HEADER_SIZE + 10..JBD2_DESCRIPTOR_HEADER_SIZE + 26],
            &superblock.s_uuid
        );
        let descriptor_checksum =
            u32::from_be_bytes(descriptor[BLOCK_SIZE - 4..].try_into().unwrap());
        assert_eq!(
            descriptor_checksum,
            reference_jbd2_block_checksum(&superblock.s_uuid, descriptor, BLOCK_SIZE - 4)
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
    fn csum_v2_64bit_tag_places_high_block_before_reserved_padding() {
        let (inner, superblock, target) =
            committed_csum_v2_fixture_with_features(JBD2_FEATURE_INCOMPAT_64BIT);
        let descriptor = &inner.data[129 * BLOCK_SIZE..130 * BLOCK_SIZE];
        let tag_offset = JBD2_DESCRIPTOR_HEADER_SIZE;
        let tag = JournalBlockTagS::from_disk_bytes(&descriptor[tag_offset..tag_offset + 8]);
        assert_eq!(tag.t_blocknr, target.raw() as u32);
        assert_eq!(
            u32::from_be_bytes(
                descriptor[tag_offset + 8..tag_offset + 12]
                    .try_into()
                    .expect("high block number")
            ),
            0
        );
        assert_eq!(&descriptor[tag_offset + 12..tag_offset + 14], &[0, 0]);
        assert_eq!(
            &descriptor[tag_offset + 14..tag_offset + 14 + JBD2_UUID_SIZE],
            &superblock.s_uuid
        );
    }

    #[test]
    fn csum_v2_replay_accepts_valid_transaction_and_rejects_payload_corruption() {
        let (inner, superblock, target) = committed_csum_v2_fixture();
        let (status, replayed) = replay_csum_v2_fixture(inner, superblock, target);
        assert_eq!(status, ReplayStatus::Complete);
        let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &replayed.data[target_start..target_start + BLOCK_SIZE],
            vec![0x6d; BLOCK_SIZE]
        );

        let (mut corrupt, superblock, target) = committed_csum_v2_fixture();
        corrupt.data[130 * BLOCK_SIZE + 64] ^= 1;
        let (status, _) = replay_csum_v2_fixture(corrupt, superblock, target);
        let failure = status
            .failure()
            .expect("corrupt v2 payload must stop replay");
        assert_eq!(failure.phase(), JournalReplayPhase::Replay);
        assert_eq!(
            failure.cause().kind(),
            crate::Ext4ErrorKind::ChecksumMismatch
        );
    }

    fn assert_csum_v2_corruption_is_rejected(corrupt: impl FnOnce(&mut Vec<u8>)) {
        let (mut inner, superblock, target) = committed_csum_v2_fixture();
        corrupt(&mut inner.data);
        let (status, _) = replay_csum_v2_fixture(inner, superblock, target);
        let failure = status.failure().expect("v2 corruption must stop replay");
        assert_eq!(
            failure.cause().kind(),
            crate::Ext4ErrorKind::ChecksumMismatch
        );
    }

    #[test]
    fn csum_v2_replay_rejects_descriptor_and_commit_corruption_before_home_write() {
        assert_csum_v2_corruption_is_rejected(|data| {
            data[130 * BLOCK_SIZE - 1] ^= 1;
        });
        assert_csum_v2_corruption_is_rejected(|data| {
            data[131 * BLOCK_SIZE + 16] ^= 1;
        });
    }

    #[test]
    fn csum_v2_replay_rejects_corrupt_revoke_tail_before_home_write() {
        let (mut inner, superblock, target) = committed_csum_v2_fixture();
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

        let (status, _) = replay_csum_v2_fixture(inner, superblock, target);
        let failure = status
            .failure()
            .expect("corrupt v2 revoke must stop replay");
        assert_eq!(failure.phase(), JournalReplayPhase::Revoke);
        assert_eq!(
            failure.cause().kind(),
            crate::Ext4ErrorKind::ChecksumMismatch
        );
    }

    #[test]
    fn compat_checksum_commit_covers_descriptor_and_payload_with_crc32_be() {
        let (inner, ..) = committed_compat_checksum_fixture();
        let descriptor = &inner.data[129 * BLOCK_SIZE..130 * BLOCK_SIZE];
        let payload = &inner.data[130 * BLOCK_SIZE..131 * BLOCK_SIZE];
        let commit = CommitHeader::from_disk_bytes(&inner.data[131 * BLOCK_SIZE..132 * BLOCK_SIZE]);
        let expected = reference_crc32_be(reference_crc32_be(u32::MAX, descriptor), payload);
        assert_eq!(commit.h_chksum_type, 1);
        assert_eq!(commit.h_chksum_size, 4);
        assert_eq!(commit.h_chksum[0], expected);
    }

    fn assert_compat_checksum_corruption_is_rejected(corrupt: impl FnOnce(&mut Vec<u8>)) {
        let (mut inner, superblock, target) = committed_compat_checksum_fixture();
        corrupt(&mut inner.data);
        let (status, _) = replay_compat_checksum_fixture(inner, superblock, target);
        let failure = status
            .failure()
            .expect("compat checksum mismatch must stop replay");
        assert_eq!(failure.phase(), JournalReplayPhase::Replay);
        assert_eq!(
            failure.cause().kind(),
            crate::Ext4ErrorKind::ChecksumMismatch
        );
    }

    #[test]
    fn compat_checksum_replay_rejects_descriptor_payload_and_commit_corruption() {
        assert_compat_checksum_corruption_is_rejected(|data| {
            data[130 * BLOCK_SIZE - 1] ^= 1;
        });
        assert_compat_checksum_corruption_is_rejected(|data| {
            data[130 * BLOCK_SIZE + 64] ^= 1;
        });
        assert_compat_checksum_corruption_is_rejected(|data| {
            data[131 * BLOCK_SIZE + 16] ^= 1;
        });
    }

    #[test]
    fn compat_checksum_replay_applies_a_valid_transaction() {
        let (inner, superblock, target) = committed_compat_checksum_fixture();
        let (status, replayed) = replay_compat_checksum_fixture(inner, superblock, target);
        assert_eq!(status, ReplayStatus::Complete);
        let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &replayed.data[target_start..target_start + BLOCK_SIZE],
            vec![0x93; BLOCK_SIZE]
        );
    }

    #[test]
    fn csum_v3_commit_escapes_magic_without_changing_checkpoint_image() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        let superblock = csum_v3_superblock();
        dev.set_journal_superblock(superblock, AbsoluteBN::new(128))
            .expect("install csum-v3 journal");
        let target = AbsoluteBN::new(10);
        let mut payload = vec![0x6b; BLOCK_SIZE];
        payload[..4].copy_from_slice(&JBD2_MAGIC.to_be_bytes());
        dev.write_blocks(&payload, target, 1, true)
            .expect("queue payload beginning with journal magic");

        dev.commit().expect("commit escaped payload");

        let descriptor = &dev.inner._device().data[129 * BLOCK_SIZE..130 * BLOCK_SIZE];
        let tag = JournalBlockTag3S::from_disk_bytes(
            &descriptor[JBD2_DESCRIPTOR_HEADER_SIZE..JBD2_DESCRIPTOR_HEADER_SIZE + JBD2_TAG3_SIZE],
        );
        assert_ne!(tag.t_flags & u32::from(JOURNAL_ESCAPE), 0);
        let journal_payload = &dev.inner._device().data[130 * BLOCK_SIZE..131 * BLOCK_SIZE];
        assert_eq!(&journal_payload[..4], &[0; 4]);
        assert_eq!(&journal_payload[4..], &payload[4..]);
        assert_eq!(
            tag.t_checksum,
            reference_jbd2_tag_checksum(&superblock.s_uuid, 1, journal_payload)
        );
        assert_eq!(
            &dev.system
                .as_ref()
                .expect("journal state")
                .checkpoint_transactions[0]
                .updates[0]
                .1[..],
            payload,
            "checkpoint must retain the unescaped home image"
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
    fn csum_v3_replay_accepts_partial_commit_block_checksum() {
        let (mut inner, superblock, target) = committed_csum_v3_fixture();
        inner.data[131 * BLOCK_SIZE + JBD2_COMMIT_HEADER_SIZE] = 0x7e;

        let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        replay_dev
            .set_journal_superblock_with_mapping(
                superblock,
                (128..192).map(AbsoluteBN::new).collect(),
            )
            .expect("install csum-v3 journal");

        assert_eq!(replay_dev.journal_replay_checked(), ReplayStatus::Complete);
        let inner = replay_dev.into_inner();
        let target_start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[target_start..target_start + BLOCK_SIZE],
            vec![0xa5; BLOCK_SIZE]
        );
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
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
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

    #[test]
    fn filesystem_sync_commits_without_checkpointing_home_metadata() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install checksummed journal");
        let target = AbsoluteBN::new(10);
        let target_offset = target.as_usize().expect("target offset") * BLOCK_SIZE;
        let payload = vec![0x73; BLOCK_SIZE];
        dev.write_blocks(&payload, target, 1, true)
            .expect("queue metadata update");

        dev.commit_for_filesystem_sync()
            .expect("commit filesystem sync transaction");

        assert!(
            dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0),
            "ordinary sync must not force committed metadata to its home block"
        );
        assert_eq!(
            dev.system
                .as_ref()
                .expect("journal state")
                .checkpoint_transactions
                .len(),
            1
        );
        assert_eq!(dev.inner._device().flush_calls, 1);
        assert_eq!(dev.inner._device().fua_writes, 1);

        dev.commit_for_filesystem_sync()
            .expect("clean filesystem sync");
        assert_eq!(
            dev.inner._device().flush_calls,
            2,
            "a sync without a transaction must still flush data writeback"
        );
    }

    #[test]
    fn journal_state_cannot_be_reinstalled_with_pending_checkpoint_owner() {
        let superblock = csum_v3_superblock();
        let journal_start = AbsoluteBN::new(128);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(superblock, journal_start)
            .expect("install checksummed journal");
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
            .expect("queue metadata update");
        dev.commit().expect("commit metadata update");

        let error = dev
            .set_journal_superblock(superblock, journal_start)
            .expect_err("reinstall must not discard a pending checkpoint owner");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
        assert_eq!(
            dev.system
                .as_ref()
                .expect("journal state")
                .checkpoint_transactions
                .len(),
            1
        );
    }

    #[test]
    fn commit_keeps_transaction_for_later_checkpoint() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let target = AbsoluteBN::new(10);
        let target_offset = target.as_usize().expect("target offset") * BLOCK_SIZE;
        let payload = vec![0x6b; BLOCK_SIZE];
        let sequence = dev.journal_sequence().expect("journal sequence");
        dev.write_blocks(&payload, target, 1, true)
            .expect("queue metadata update");

        dev.commit().expect("commit running transaction");

        assert_eq!(dev.journal_sequence(), Some(sequence.wrapping_add(1)));
        let system = dev.system.as_ref().expect("journal state");
        assert!(system.running_transaction.updates.is_empty());
        assert!(system.committing_transaction.is_none());
        assert_eq!(system.checkpoint_transactions.len(), 1);
        assert_ne!(
            system.jbd2_super_block.s_start, 0,
            "the oldest committed transaction must remain discoverable"
        );
        assert!(
            dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0),
            "commit must not synchronously checkpoint the home block"
        );

        let mut visible = vec![0; BLOCK_SIZE];
        dev.read_blocks(&mut visible, target, 1)
            .expect("read committed metadata through journal owner");
        assert_eq!(visible, payload);

        dev.flush().expect("checkpoint committed transaction");
        assert_eq!(
            &dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE],
            payload
        );
        assert_eq!(
            dev.system
                .as_ref()
                .expect("journal state")
                .jbd2_super_block
                .s_start,
            0,
            "checkpoint must reclaim the journal tail"
        );
    }

    #[test]
    fn commit_record_uses_fua_after_descriptor_flush() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install checksummed journal");
        dev.write_blocks(&vec![0x67; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
            .expect("queue metadata update");

        dev.commit().expect("commit metadata transaction");

        assert_eq!(
            dev.inner._device().fua_writes,
            1,
            "the commit record must be the transaction's FUA publication"
        );
        assert_eq!(
            dev.inner._device().flush_calls,
            1,
            "the pre-commit flush orders descriptor and payload writes before the FUA commit"
        );
    }

    #[test]
    fn checkpoint_reclaims_only_oldest_committed_transaction() {
        let superblock = csum_v3_superblock();
        let journal_start = AbsoluteBN::new(128);
        let first_target = AbsoluteBN::new(10);
        let second_target = AbsoluteBN::new(11);
        let first_payload = vec![0x51; BLOCK_SIZE];
        let second_payload = vec![0xa6; BLOCK_SIZE];
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(superblock, journal_start)
            .expect("install checksummed journal");

        dev.write_blocks(&first_payload, first_target, 1, true)
            .expect("queue first transaction");
        dev.commit().expect("commit first transaction");
        dev.write_blocks(&second_payload, second_target, 1, true)
            .expect("queue second transaction");
        dev.commit().expect("commit second transaction");

        dev.checkpoint_pending_transactions()
            .expect("checkpoint only the oldest transaction");

        let first_offset = first_target.as_usize().expect("first target") * BLOCK_SIZE;
        let second_offset = second_target.as_usize().expect("second target") * BLOCK_SIZE;
        assert_eq!(
            &dev.inner._device().data[first_offset..first_offset + BLOCK_SIZE],
            first_payload
        );
        assert!(
            dev.inner._device().data[second_offset..second_offset + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0),
            "a single checkpoint step must leave the later home image pending"
        );
        let system = dev.system.as_ref().expect("journal state");
        assert_eq!(system.checkpoint_transactions.len(), 1);
        assert_eq!(system.checkpoint_transactions[0].sequence, 2);
        assert_ne!(system.jbd2_super_block.s_start, 0);
        assert_eq!(system.jbd2_super_block.s_sequence, 2);

        let replay_superblock = system.jbd2_super_block;
        let inner = dev.into_inner();
        let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        replay_dev
            .set_journal_superblock(replay_superblock, journal_start)
            .expect("install advanced journal tail");
        assert_eq!(replay_dev.journal_replay_checked(), ReplayStatus::Complete);
        let inner = replay_dev.into_inner();
        assert_eq!(
            &inner.data[second_offset..second_offset + BLOCK_SIZE],
            second_payload,
            "the later committed transaction must remain replayable"
        );
    }

    #[test]
    fn flush_batches_committed_transactions_into_one_tail_fua() {
        let superblock = csum_v3_superblock();
        let journal_start = AbsoluteBN::new(128);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(superblock, journal_start)
            .expect("install checksummed journal");

        dev.write_blocks(&vec![0x51; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
            .expect("queue first transaction");
        dev.commit().expect("commit first transaction");
        dev.write_blocks(&vec![0xa6; BLOCK_SIZE], AbsoluteBN::new(11), 1, true)
            .expect("queue second transaction");
        dev.commit().expect("commit second transaction");
        assert_eq!(dev.inner._device().fua_writes, 2);

        dev.flush()
            .expect("checkpoint every committed transaction as one batch");

        assert_eq!(
            dev.inner._device().fua_writes,
            3,
            "one flush batch must publish the final journal tail with one FUA"
        );
        assert!(
            dev.system
                .as_ref()
                .expect("journal state")
                .checkpoint_transactions
                .is_empty()
        );
    }

    #[test]
    fn checkpoint_batch_writes_only_latest_home_block_version() {
        let superblock = csum_v3_superblock();
        let journal_start = AbsoluteBN::new(128);
        let target = AbsoluteBN::new(10);
        let latest_payload = vec![0xa6; BLOCK_SIZE];
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(superblock, journal_start)
            .expect("install checksummed journal");

        dev.write_blocks(&vec![0x51; BLOCK_SIZE], target, 1, true)
            .expect("queue first transaction");
        dev.commit().expect("commit first transaction");
        dev.write_blocks(&latest_payload, target, 1, true)
            .expect("queue replacement transaction");
        dev.commit().expect("commit replacement transaction");
        dev.inner._device_mut().write_calls = 0;

        dev.flush()
            .expect("checkpoint every committed transaction as one batch");

        assert_eq!(
            dev.inner._device().write_calls,
            2,
            "checkpoint must write one latest home image and one tail superblock"
        );
        let target_offset = target.as_usize().expect("target") * BLOCK_SIZE;
        assert_eq!(
            &dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE],
            latest_payload
        );
    }

    #[test]
    fn failed_tail_fua_preserves_oldest_replay_boundary() {
        let superblock = csum_v3_superblock();
        let journal_start = AbsoluteBN::new(128);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(superblock, journal_start)
            .expect("install checksummed journal");
        dev.write_blocks(&vec![0x37; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
            .expect("queue first transaction");
        dev.commit().expect("commit first transaction");
        dev.write_blocks(&vec![0x92; BLOCK_SIZE], AbsoluteBN::new(11), 1, true)
            .expect("queue second transaction");
        dev.commit().expect("commit second transaction");

        let (tail_sequence, tail_start, used_records) = {
            let system = dev.system.as_ref().expect("journal state");
            (
                system.jbd2_super_block.s_sequence,
                system.jbd2_super_block.s_start,
                system.used_log_records,
            )
        };
        dev.inner._device_mut().fail_fua = true;

        let error = dev
            .checkpoint_pending_transactions()
            .expect_err("tail FUA failure must abort checkpoint");

        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);
        let system = dev.system.as_ref().expect("journal state");
        assert_eq!(system.jbd2_super_block.s_sequence, tail_sequence);
        assert_eq!(system.jbd2_super_block.s_start, tail_start);
        assert_eq!(system.used_log_records, used_records);
        assert_eq!(system.checkpoint_transactions.len(), 2);
    }

    #[test]
    fn partial_checkpoint_reuses_wrapped_log_without_losing_later_transactions() {
        let superblock = small_journal_superblock();
        let journal_start = AbsoluteBN::new(128);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(superblock, journal_start)
            .expect("install small journal");

        for transaction in 0..4u32 {
            dev.write_blocks(
                &vec![0x40 + transaction as u8; BLOCK_SIZE],
                AbsoluteBN::new(10 + u64::from(transaction)),
                1,
                true,
            )
            .expect("queue transaction before wrap");
            dev.commit().expect("commit transaction before wrap");
        }
        dev.checkpoint_pending_transactions()
            .expect("reclaim first transaction");
        dev.write_blocks(&vec![0x44; BLOCK_SIZE], AbsoluteBN::new(14), 1, true)
            .expect("queue transaction at ring end");
        dev.commit().expect("commit transaction at ring end");
        dev.checkpoint_pending_transactions()
            .expect("reclaim second transaction");
        dev.write_blocks(&vec![0x45; BLOCK_SIZE], AbsoluteBN::new(15), 1, true)
            .expect("queue wrapped transaction");
        dev.commit().expect("commit wrapped transaction");

        let system = dev.system.as_ref().expect("journal state");
        assert_eq!(system.checkpoint_transactions.len(), 4);
        assert_eq!(system.jbd2_super_block.s_sequence, 3);
        assert_eq!(system.jbd2_super_block.s_start, 7);
        assert_eq!(system.head, 4);
        let replay_superblock = system.jbd2_super_block;
        let inner = dev.into_inner();
        let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        replay_dev
            .set_journal_superblock(replay_superblock, journal_start)
            .expect("install partial-checkpoint tail");
        assert_eq!(replay_dev.journal_replay_checked(), ReplayStatus::Complete);
        let inner = replay_dev.into_inner();
        for transaction in 0..6u32 {
            let offset = (10 + transaction) as usize * BLOCK_SIZE;
            assert_eq!(
                inner.data[offset],
                0x40 + transaction as u8,
                "transaction {transaction} must survive checkpoint and replay"
            );
        }
    }

    #[test]
    fn later_writer_revoke_preserves_reused_home_block_after_replay() {
        let superblock = csum_v3_superblock();
        let journal_start = AbsoluteBN::new(128);
        let target = AbsoluteBN::new(10);
        let target_offset = target.as_usize().expect("target offset") * BLOCK_SIZE;
        let old_metadata = vec![0x41; BLOCK_SIZE];
        let new_owner = vec![0xb7; BLOCK_SIZE];
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(superblock, journal_start)
            .expect("install small journal");

        dev.write_blocks(&old_metadata, target, 1, true)
            .expect("queue old metadata image");
        dev.commit().expect("commit old metadata transaction");
        dev.with_transaction_credits(TransactionCredits::metadata_with_revokes(0, 1), |dev| {
            dev.forget_detached_metadata(target)?;
            dev.write_blocks(&new_owner, target, 1, false)
        })
        .expect("detach metadata and reuse its block in one transaction");
        dev.commit().expect("commit later revoke transaction");

        let inner = dev.into_inner();
        assert_eq!(
            &inner.data[target_offset..target_offset + BLOCK_SIZE],
            new_owner,
            "new owner must reach its home block before the revoke commit"
        );
        let revoke_offset = (journal_start.as_usize().expect("journal offset") + 4) * BLOCK_SIZE;
        let revoke_block = &inner.data[revoke_offset..revoke_offset + BLOCK_SIZE];
        let revoke = Jbd2JournalRevokeHeadS::from_disk_bytes(revoke_block);
        assert_eq!(revoke.r_header.h_blocktype, JBD2_BLOCKTYPE_REVOKE);
        assert_eq!(revoke.r_header.h_sequence, 2);
        assert_eq!(
            revoke.r_count, 24,
            "64-bit revoke must contain one u64 entry"
        );
        assert_eq!(
            u64::from_be_bytes(revoke_block[16..24].try_into().expect("revoke entry")),
            target.raw()
        );
        assert_eq!(
            u32::from_be_bytes(
                revoke_block[BLOCK_SIZE - 4..]
                    .try_into()
                    .expect("revoke checksum")
            ),
            reference_jbd2_block_checksum(&superblock.s_uuid, revoke_block, BLOCK_SIZE - 4)
        );

        let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        let mut replay_superblock = superblock;
        replay_superblock.s_start = replay_superblock.s_first;
        replay_superblock.s_sequence = 1;
        crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
        replay_dev
            .set_journal_superblock(replay_superblock, journal_start)
            .expect("restore pre-crash journal state");
        assert_eq!(replay_dev.journal_replay_checked(), ReplayStatus::Complete);

        let inner = replay_dev.into_inner();
        assert_eq!(
            &inner.data[target_offset..target_offset + BLOCK_SIZE],
            new_owner,
            "the later revoke must suppress replay of the detached metadata image"
        );
    }

    #[test]
    fn later_writer_revoke_suppresses_older_checkpoint_write() {
        let superblock = csum_v3_superblock();
        let journal_start = AbsoluteBN::new(128);
        let target = AbsoluteBN::new(10);
        let target_offset = target.as_usize().expect("target offset") * BLOCK_SIZE;
        let old_metadata = vec![0x31; BLOCK_SIZE];
        let new_owner = vec![0xc4; BLOCK_SIZE];
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(superblock, journal_start)
            .expect("install checksummed journal");

        dev.write_blocks(&old_metadata, target, 1, true)
            .expect("queue old metadata image");
        dev.commit().expect("commit old metadata transaction");
        dev.with_transaction_credits(TransactionCredits::metadata_with_revokes(0, 1), |dev| {
            dev.forget_detached_metadata(target)?;
            dev.write_blocks(&new_owner, target, 1, false)
        })
        .expect("detach metadata and reuse its block");
        dev.commit().expect("commit revoke transaction");

        dev.flush().expect("checkpoint both committed transactions");

        assert_eq!(
            &dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE],
            new_owner,
            "checkpoint must skip an older image covered by a later revoke"
        );
        assert_eq!(
            dev.system
                .as_ref()
                .expect("journal state")
                .jbd2_super_block
                .s_start,
            0
        );
    }

    #[test]
    fn metadata_reuse_cancels_same_transaction_revoke() {
        let superblock = csum_v3_superblock();
        let journal_start = AbsoluteBN::new(128);
        let target = AbsoluteBN::new(10);
        let target_offset = target.as_usize().expect("target offset") * BLOCK_SIZE;
        let old_metadata = vec![0x21; BLOCK_SIZE];
        let new_metadata = vec![0xd9; BLOCK_SIZE];
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(superblock, journal_start)
            .expect("install checksummed journal");

        dev.write_blocks(&old_metadata, target, 1, true)
            .expect("queue old metadata image");
        dev.commit().expect("commit old metadata transaction");
        dev.with_transaction_credits(TransactionCredits::metadata_with_revokes(1, 1), |dev| {
            dev.forget_detached_metadata(target)?;
            dev.write_blocks(&new_metadata, target, 1, true)
        })
        .expect("reuse metadata block in the running transaction");
        dev.commit().expect("commit replacement metadata");

        let inner = dev.into_inner();
        assert!(
            inner.data[target_offset..target_offset + BLOCK_SIZE]
                .iter()
                .all(|byte| *byte == 0),
            "metadata must remain journal-only before checkpoint"
        );
        let mut replay_dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        let mut replay_superblock = superblock;
        replay_superblock.s_start = replay_superblock.s_first;
        replay_superblock.s_sequence = 1;
        crate::checksum::jbd2_update_superblock_checksum(&mut replay_superblock);
        replay_dev
            .set_journal_superblock(replay_superblock, journal_start)
            .expect("restore pre-crash journal state");
        assert_eq!(replay_dev.journal_replay_checked(), ReplayStatus::Complete);

        let inner = replay_dev.into_inner();
        assert_eq!(
            &inner.data[target_offset..target_offset + BLOCK_SIZE],
            new_metadata,
            "journaling the reused block must cancel its same-transaction revoke"
        );
    }

    #[test]
    fn existing_running_revoke_is_accounted_before_handle_reservation() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let sequence = dev.journal_sequence().expect("journal sequence");
        dev.forget_detached_metadata(AbsoluteBN::new(10))
            .expect("queue standalone revoke");

        let capacity = dev
            .journal_transaction_capacity()
            .expect("small journal capacity");
        dev.with_transaction_handle(capacity, |dev| {
            for offset in 0..capacity {
                let target = AbsoluteBN::new(
                    20u64
                        .checked_add(u64::try_from(offset).map_err(|_| Ext4Error::overflow())?)
                        .ok_or_else(Ext4Error::overflow)?,
                );
                dev.write_blocks(&vec![offset as u8; BLOCK_SIZE], target, 1, true)?;
            }
            Ok(())
        })
        .expect("reserve a full metadata handle after closing the revoke transaction");

        assert_eq!(
            dev.journal_sequence(),
            Some(sequence.wrapping_add(1)),
            "the running revoke must be committed before the full handle starts"
        );
        dev.commit().expect("commit full metadata handle");
        dev.flush().expect("checkpoint both transactions");
        for offset in 0..capacity {
            let target_offset = (20 + offset) * BLOCK_SIZE;
            assert_eq!(
                &dev.inner._device().data[target_offset..target_offset + BLOCK_SIZE],
                vec![offset as u8; BLOCK_SIZE]
            );
        }
    }

    #[test]
    fn standalone_revoke_reserves_log_space_before_mutating_the_running_transaction() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        let constrained_ring = JournalSuperBlock {
            s_maxlen: 16,
            s_first: 13,
            ..JournalSuperBlock::default()
        };
        dev.set_journal_superblock(constrained_ring, AbsoluteBN::new(128))
            .expect("install journal whose ring is smaller than one maximum transaction");

        let error = dev
            .forget_detached_metadata(AbsoluteBN::new(10))
            .expect_err("revoke must not start without maximum-transaction log space");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
        assert!(
            dev.system
                .as_ref()
                .unwrap()
                .running_transaction
                .revoked_blocks
                .is_empty(),
            "failed start-time reservation must not publish a revoke"
        );
    }

    fn assert_commit_stage_fault_aborts_journal(device: MemBlockDev, stage: &str) {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, device, true);
        dev.set_journal_superblock(csum_v3_superblock(), AbsoluteBN::new(128))
            .expect("install checksummed journal");
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
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
        let expected_fua_writes = match stage {
            "open-superblock" | "descriptor" | "payload" | "descriptor-payload-barrier" => 1,
            "commit" | "checkpoint" | "checkpoint-barrier" => 2,
            "close-superblock" => 3,
            _ => panic!("unknown commit fault stage: {stage}"),
        };
        assert_eq!(
            dev.inner._device().fua_writes,
            expected_fua_writes,
            "{stage}"
        );
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

        let flush_stages = ["descriptor-payload-barrier", "checkpoint-barrier"];
        for (index, stage) in flush_stages.iter().enumerate() {
            assert_commit_stage_fault_aborts_journal(
                MemBlockDev::with_failing_flush_call(256, index + 1),
                stage,
            );
        }
    }

    #[test]
    fn commit_failure_aborts_future_journal_operations() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::with_failing_flush(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
            .expect("queue metadata update");

        let first_error = dev
            .umount_commit()
            .expect_err("first failed commit must propagate the device error");
        assert_eq!(first_error.kind(), crate::Ext4ErrorKind::Io);
        let system = dev.system.as_ref().expect("journal state");
        assert!(
            system.running_transaction.updates.is_empty(),
            "a transaction that entered commit I/O must no longer be owned by the running queue"
        );
        let committing = system
            .committing_transaction
            .as_ref()
            .expect("failed transaction remains owned by the committing state");
        assert_eq!(committing.updates.len(), 1);
        assert_eq!(committing.phase, Jbd2CommitPhase::DataFlush);

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
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
            .expect("queue metadata update");

        let first_error = dev
            .umount_commit()
            .expect_err("first failed commit must propagate the device error");
        assert_eq!(first_error.kind(), crate::Ext4ErrorKind::Io);

        let inner = dev.into_inner();
        assert_eq!(inner.fua_writes, 1, "abort errno must use one FUA write");
        let journal_offset = 128 * BLOCK_SIZE;
        let recorded =
            JournalSuperBlock::from_disk_bytes(&inner.data[journal_offset..journal_offset + 1024]);
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
            .write_blocks(
                &vec![0x5a; BLOCK_SIZE],
                AbsoluteBN::new(10 + capacity as u64),
                1,
                true,
            )
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
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
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
    fn failed_journal_handle_does_not_write_dirty_metadata_cache_home() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let target = AbsoluteBN::new(10);
        dev.read_block(target).expect("cache clean home block");

        let error = dev
            .with_journal_handle(1, |dev| {
                dev.buffer_mut()[0] = 0x5a;
                dev.write_block(target, true)?;
                Err::<(), _>(Ext4Error::io())
            })
            .expect_err("operation failure must abort the handle update");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

        let inner = dev.into_inner();
        let start = target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            inner.data[start], 0,
            "aborted journal metadata reached its home block"
        );
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
    fn new_handle_checkpoints_before_dirtying_when_log_lacks_maximum_transaction_space() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let capacity = dev.journal_transaction_capacity().unwrap();
        let maximum_records = dev.journal_maximum_transaction_records().unwrap();
        assert_eq!((capacity, maximum_records), (3, 5));

        for transaction in 0..3u64 {
            let first_target = AbsoluteBN::new(10 + transaction * capacity as u64);
            dev.with_journal_handle(capacity, |dev| {
                for offset in 0..capacity {
                    dev.write_blocks(
                        &vec![transaction as u8 + 1; BLOCK_SIZE],
                        first_target.checked_add(u32::try_from(offset).unwrap())?,
                        1,
                        true,
                    )?;
                }
                Ok(())
            })
            .expect("fill one maximum-sized transaction");
            dev.commit().expect("commit maximum-sized transaction");
        }
        assert_eq!(dev.journal_available_log_records().unwrap(), 0);
        assert_eq!(
            dev.system.as_ref().unwrap().checkpoint_transactions.len(),
            3
        );

        dev.with_journal_handle(1, |dev| {
            assert_eq!(dev.journal_available_log_records()?, maximum_records);
            assert_eq!(
                dev.system.as_ref().unwrap().checkpoint_transactions.len(),
                2,
                "space must be reclaimed before the operation can dirty metadata"
            );
            assert_eq!(
                dev.extend_transaction_credits(TransactionCredits::metadata(capacity - 1))?,
                TransactionHandleExtension::Extended,
                "extend uses the already guaranteed log reservation"
            );
            Ok(())
        })
        .expect("a new handle must reserve one maximum transaction of log space");
    }

    #[test]
    fn unscoped_metadata_write_reserves_log_space_before_starting_a_transaction() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let capacity = dev.journal_transaction_capacity().unwrap();
        for transaction in 0..3u64 {
            let first_target = AbsoluteBN::new(10 + transaction * capacity as u64);
            dev.with_journal_handle(capacity, |dev| {
                for offset in 0..capacity {
                    dev.write_blocks(
                        &vec![transaction as u8 + 1; BLOCK_SIZE],
                        first_target.checked_add(u32::try_from(offset).unwrap())?,
                        1,
                        true,
                    )?;
                }
                Ok(())
            })
            .expect("fill one maximum-sized transaction");
            dev.commit().expect("commit maximum-sized transaction");
        }
        assert_eq!(dev.journal_available_log_records().unwrap(), 0);

        dev.write_blocks(&vec![0x7e; BLOCK_SIZE], AbsoluteBN::new(64), 1, true)
            .expect("unscoped write must reclaim space before starting a transaction");
        assert_eq!(dev.journal_available_log_records().unwrap(), 5);
        assert_eq!(
            dev.system.as_ref().unwrap().checkpoint_transactions.len(),
            2
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
    fn journal_handle_extends_before_touching_an_additional_block() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let first = AbsoluteBN::new(10);
        let second = AbsoluteBN::new(11);

        dev.with_journal_handle(1, |dev| {
            dev.write_blocks(&vec![0x31; BLOCK_SIZE], first, 1, true)?;
            assert_eq!(
                dev.extend_transaction_credits(TransactionCredits::metadata(1))?,
                TransactionHandleExtension::Extended
            );
            dev.write_blocks(&vec![0x42; BLOCK_SIZE], second, 1, true)
        })
        .expect("extended handle must reserve the second metadata block");

        dev.umount_commit().expect("commit extended handle update");
        let inner = dev.into_inner();
        let first_start = first.as_usize().unwrap() * BLOCK_SIZE;
        let second_start = second.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[first_start..first_start + BLOCK_SIZE],
            &vec![0x31; BLOCK_SIZE]
        );
        assert_eq!(
            &inner.data[second_start..second_start + BLOCK_SIZE],
            &vec![0x42; BLOCK_SIZE]
        );
    }

    #[test]
    fn transaction_restart_switches_before_attaching_the_next_handle() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let first = AbsoluteBN::new(10);
        let second = AbsoluteBN::new(11);

        dev.with_transaction_handle(1, |dev| {
            dev.write_blocks(&vec![0x35; BLOCK_SIZE], first, 1, true)
        })
        .expect("publish the old transaction step");
        let old_sequence = dev.journal_sequence().expect("old transaction sequence");

        dev.restart_transaction(TransactionCredits::metadata(1), |dev| {
            assert_ne!(
                dev.journal_sequence(),
                Some(old_sequence),
                "restart must switch the old transaction before the new handle attaches"
            );
            dev.write_blocks(&vec![0x46; BLOCK_SIZE], second, 1, true)
        })
        .expect("restart into the next transaction");

        let system = dev.system.as_ref().expect("journal state");
        assert_eq!(system.checkpoint_transactions.len(), 1);
        assert_eq!(system.checkpoint_transactions[0].updates.len(), 1);
        assert_eq!(system.running_transaction.updates.len(), 1);
        dev.umount_commit().expect("commit both transaction steps");
        let inner = dev.into_inner();
        let first_start = first.as_usize().unwrap() * BLOCK_SIZE;
        let second_start = second.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[first_start..first_start + BLOCK_SIZE],
            &vec![0x35; BLOCK_SIZE]
        );
        assert_eq!(
            &inner.data[second_start..second_start + BLOCK_SIZE],
            &vec![0x46; BLOCK_SIZE]
        );
    }

    #[test]
    fn transaction_restart_preserves_a_detached_reserved_handle() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let first = AbsoluteBN::new(10);
        let second = AbsoluteBN::new(11);
        let third = AbsoluteBN::new(12);
        let ((), reserved) = dev
            .with_transaction_reservation(
                TransactionCredits::metadata(1),
                TransactionCredits::metadata(1),
                |dev| dev.write_blocks(&vec![0x51; BLOCK_SIZE], first, 1, true),
            )
            .expect("publish parent and detach child reservation");
        let old_sequence = dev.journal_sequence().expect("old transaction sequence");

        dev.restart_transaction(TransactionCredits::metadata(1), |dev| {
            assert_ne!(dev.journal_sequence(), Some(old_sequence));
            dev.write_blocks(&vec![0x62; BLOCK_SIZE], second, 1, true)
        })
        .expect("restart while retaining detached child credits");
        let new_sequence = dev.journal_sequence().expect("new transaction sequence");
        dev.with_reserved_transaction(reserved, |dev| {
            assert_eq!(dev.journal_sequence(), Some(new_sequence));
            dev.write_blocks(&vec![0x73; BLOCK_SIZE], third, 1, true)
        })
        .expect("attach child to the restarted transaction");

        dev.umount_commit().expect("commit restarted transaction");
        let inner = dev.into_inner();
        for (block, byte) in [(first, 0x51), (second, 0x62), (third, 0x73)] {
            let start = block.as_usize().unwrap() * BLOCK_SIZE;
            assert_eq!(
                &inner.data[start..start + BLOCK_SIZE],
                &vec![byte; BLOCK_SIZE]
            );
        }
    }

    #[test]
    fn transaction_restart_rejects_an_active_handle_without_committing_it() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let sequence = dev.journal_sequence().expect("journal sequence");

        dev.with_transaction_handle(1, |dev| {
            let mut operation_started = false;
            let error = dev
                .restart_transaction(TransactionCredits::metadata(1), |_| {
                    operation_started = true;
                    Ok(())
                })
                .expect_err("restart must begin only after the old handle stops");
            assert!(!operation_started);
            assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
            assert_eq!(dev.journal_sequence(), Some(sequence));
            Ok(())
        })
        .expect("outer handle remains valid");
    }

    #[test]
    fn failed_journal_handle_extension_preserves_the_original_reservation() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let first = AbsoluteBN::new(10);
        let second = AbsoluteBN::new(11);

        dev.with_journal_handle(1, |dev| {
            dev.write_blocks(&vec![0x53; BLOCK_SIZE], first, 1, true)?;
            assert_eq!(
                dev.extend_transaction_credits(TransactionCredits::metadata(usize::MAX))?,
                TransactionHandleExtension::RestartRequired
            );
            let error = dev
                .write_blocks(&vec![0x64; BLOCK_SIZE], second, 1, true)
                .expect_err("failed extension must not change the original one-credit handle");
            assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);
            Ok(())
        })
        .expect("the original handle remains valid after best-effort extension fails");

        dev.umount_commit().expect("commit original handle update");
        let inner = dev.into_inner();
        let first_start = first.as_usize().unwrap() * BLOCK_SIZE;
        let second_start = second.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[first_start..first_start + BLOCK_SIZE],
            &vec![0x53; BLOCK_SIZE]
        );
        assert_eq!(
            &inner.data[second_start..second_start + BLOCK_SIZE],
            &vec![0; BLOCK_SIZE]
        );
    }

    #[test]
    fn journal_handle_extension_accounts_for_the_existing_running_transaction() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        dev.write_blocks(&vec![0x17; BLOCK_SIZE], AbsoluteBN::new(9), 1, true)
            .expect("queue metadata before starting the handle");

        dev.with_journal_handle(1, |dev| {
            dev.write_blocks(&vec![0x28; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)?;
            assert_eq!(
                dev.extend_transaction_credits(TransactionCredits::metadata(1))?,
                TransactionHandleExtension::Extended
            );
            assert_eq!(
                dev.extend_transaction_credits(TransactionCredits::metadata(1))?,
                TransactionHandleExtension::RestartRequired,
                "the pre-handle update must remain part of the reservation"
            );
            Ok(())
        })
        .expect("the full transaction capacity must remain usable");
    }

    #[test]
    fn direct_metadata_handle_extension_expands_the_rollback_owner() {
        let first = AbsoluteBN::new(10);
        let second = AbsoluteBN::new(11);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), false);

        let error = dev
            .with_transaction_handle(1, |dev| {
                dev.write_blocks(&vec![0x39; BLOCK_SIZE], first, 1, true)?;
                assert_eq!(
                    dev.extend_transaction_credits(TransactionCredits::metadata(1))?,
                    TransactionHandleExtension::Extended
                );
                dev.write_blocks(&vec![0x4a; BLOCK_SIZE], second, 1, true)?;
                Err::<(), _>(Ext4Error::io().with_operation("test:direct_extended_abort"))
            })
            .expect_err("operation failure must restore every extended direct-write block");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

        let mut first_after = vec![0xff; BLOCK_SIZE];
        let mut second_after = vec![0xff; BLOCK_SIZE];
        dev.read_blocks(&mut first_after, first, 1)
            .expect("read restored first direct block");
        dev.read_blocks(&mut second_after, second, 1)
            .expect("read restored second direct block");
        assert_eq!(first_after, vec![0; BLOCK_SIZE]);
        assert_eq!(second_after, vec![0; BLOCK_SIZE]);
    }

    #[test]
    fn nested_journal_handle_reuses_outer_credits_and_transaction() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let outer_target = AbsoluteBN::new(10);
        let nested_target = AbsoluteBN::new(11);
        let sequence = dev.journal_sequence();

        dev.with_journal_handle(2, |dev| {
            dev.write_blocks(&vec![0x5a; BLOCK_SIZE], outer_target, 1, true)?;
            dev.with_journal_handle(usize::MAX, |dev| {
                assert_eq!(dev.journal_sequence(), sequence);
                dev.write_blocks(&vec![0xa5; BLOCK_SIZE], nested_target, 1, true)
            })
        })
        .expect("nested start reuses the current owner and its credits");

        assert_eq!(dev.journal_sequence(), sequence);
        dev.umount_commit().expect("commit outer handle update");
        let inner = dev.into_inner();
        let start = outer_target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[start..start + BLOCK_SIZE],
            &vec![0x5a; BLOCK_SIZE]
        );
        let start = nested_target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[start..start + BLOCK_SIZE],
            &vec![0xa5; BLOCK_SIZE]
        );
    }

    #[test]
    fn failed_nested_journal_handle_restores_only_its_scope() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");
        let outer_target = AbsoluteBN::new(10);
        let failed_target = AbsoluteBN::new(11);

        dev.with_journal_handle(2, |dev| {
            dev.write_blocks(&vec![0x11; BLOCK_SIZE], outer_target, 1, true)?;
            let error = dev
                .with_journal_handle(1, |dev| {
                    dev.write_blocks(&vec![0xa5; BLOCK_SIZE], failed_target, 1, true)?;
                    Err::<(), _>(Ext4Error::io().with_operation("test:nested_operation_failure"))
                })
                .expect_err("nested operation must propagate its failure");
            assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

            let mut observed = vec![0xff; BLOCK_SIZE];
            dev.read_blocks(&mut observed, failed_target, 1)?;
            assert_eq!(observed, vec![0; BLOCK_SIZE]);
            dev.write_blocks(&vec![0x22; BLOCK_SIZE], outer_target, 1, true)
        })
        .expect("outer handle remains usable after nested rollback");

        dev.umount_commit().expect("commit outer handle update");
        let inner = dev.into_inner();
        let outer_start = outer_target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[outer_start..outer_start + BLOCK_SIZE],
            &vec![0x22; BLOCK_SIZE]
        );
        let failed_start = failed_target.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            &inner.data[failed_start..failed_start + BLOCK_SIZE],
            &vec![0; BLOCK_SIZE]
        );
    }

    #[test]
    fn failed_nested_handle_restores_revoke_credits_and_table() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install small journal");

        dev.with_transaction_credits(TransactionCredits::metadata_with_revokes(0, 2), |dev| {
            let error = dev
                .with_journal_handle(usize::MAX, |dev| {
                    dev.forget_detached_metadata(AbsoluteBN::new(10))?;
                    Err::<(), _>(Ext4Error::io().with_operation("test:nested_revoke_abort"))
                })
                .expect_err("nested revoke failure must roll back its scope");
            assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);
            assert!(
                dev.system
                    .as_ref()
                    .unwrap()
                    .running_transaction
                    .revoked_blocks
                    .is_empty()
            );
            assert_eq!(
                dev.active_handle.as_ref().unwrap().revoke_credits_remaining,
                2
            );
            dev.forget_detached_metadata(AbsoluteBN::new(10))?;
            dev.forget_detached_metadata(AbsoluteBN::new(11))
        })
        .expect("the outer handle must retain both revoke records");

        dev.commit().expect("commit restored outer revoke scope");
        assert_eq!(
            dev.system.as_ref().unwrap().checkpoint_transactions[0]
                .revoked_blocks
                .len(),
            2
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
            assert_eq!(
                dev.system.as_ref().unwrap().running_transaction.phase,
                crate::jbd2::jbdstruct::Jbd2RunningTransactionPhase::Running
            );
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
            assert_eq!(
                dev.system.as_ref().unwrap().running_transaction.phase,
                crate::jbd2::jbdstruct::Jbd2RunningTransactionPhase::Running,
                "a rejected commit must not lock the active transaction"
            );
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
    #[test]
    fn umount_commit_returns_journal_superblock_write_failure_without_panicking() {
        let journal_superblock = AbsoluteBN::new(128);
        let mut inner = MemBlockDev::new(256);
        inner.fail_next_write_at_block(journal_superblock);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        dev.set_journal_superblock(small_journal_superblock(), journal_superblock)
            .expect("install journal state");
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
            .expect("queue metadata update");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dev.umount_commit()));

        assert!(result.is_ok(), "journal I/O failure must not panic");
        assert_eq!(result.unwrap(), Err(Ext4Error::io()));
    }

    #[test]
    fn umount_commit_rejects_an_unfinished_edit_without_aborting_the_journal() {
        let cached_block = AbsoluteBN::new(20);
        let mut inner = MemBlockDev::new(256);
        inner.fail_next_write_at_block(cached_block);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install journal state");
        dev.read_block(cached_block).expect("prime cached block");
        dev.buffer_mut()[0] = 1;
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], AbsoluteBN::new(10), 1, true)
            .expect("queue metadata update");

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dev.umount_commit()));

        assert!(result.is_ok(), "an unfinished edit must not panic");
        assert_eq!(
            result.unwrap().unwrap_err().kind(),
            crate::Ext4ErrorKind::Busy
        );

        dev.inner.discard_held();
        dev.umount_commit()
            .expect("discarding the unpublished edit keeps the journal usable");
    }

    #[test]
    fn commit_rejects_an_unfinished_block_edit_before_publishing_it_to_home() {
        let unfinished_block = AbsoluteBN::new(20);
        let journaled_block = AbsoluteBN::new(10);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install journal state");
        let sequence = dev.journal_sequence();

        dev.read_block(unfinished_block).expect("prime cache");
        dev.buffer_mut()[0] = 1;
        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], journaled_block, 1, true)
            .expect("queue an unrelated journal update");

        let error = dev
            .commit()
            .expect_err("an unfinished edit must not bypass the journal");

        assert_eq!(error.kind(), crate::Ext4ErrorKind::Busy);
        assert_eq!(dev.journal_sequence(), sequence);
        let offset = unfinished_block.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            dev.inner._device().data[offset],
            0,
            "the unfinished cache image must not reach the home block"
        );
    }

    #[test]
    fn failed_block_edit_discards_the_unpublished_image() {
        let edited_block = AbsoluteBN::new(20);
        let journaled_block = AbsoluteBN::new(10);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);
        dev.set_journal_superblock(small_journal_superblock(), AbsoluteBN::new(128))
            .expect("install journal state");

        let error = dev
            .update_block(edited_block, true, |image| {
                image[0] = 1;
                Err::<(), _>(Ext4Error::io())
            })
            .expect_err("the edit closure must propagate its error");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

        dev.write_blocks(&vec![0x5a; BLOCK_SIZE], journaled_block, 1, true)
            .expect("queue an unrelated journal update");
        dev.umount_commit()
            .expect("the discarded edit must not block a later commit");

        let offset = edited_block.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            dev.inner._device().data[offset],
            0,
            "a failed edit must not reach the journal or home block"
        );
    }

    #[test]
    fn failed_block_edit_publish_discards_the_unpublished_image() {
        let edited_block = AbsoluteBN::new(20);
        let mut inner = MemBlockDev::new(256);
        inner.fail_next_write_at_block(edited_block);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, inner, false);

        let error = dev
            .update_block(edited_block, true, |image| {
                image[0] = 1;
                Ok(())
            })
            .expect_err("the direct publish failure must propagate");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::Io);

        dev.flush()
            .expect("the failed edit must not leave a dirty cache image");
        let offset = edited_block.as_usize().unwrap() * BLOCK_SIZE;
        assert_eq!(
            dev.inner._device().data[offset],
            0,
            "a failed direct publish must not be retried by cache writeback"
        );
    }

    #[test]
    fn rejects_an_empty_journal_mapping() {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), true);

        let error = dev
            .set_journal_superblock_with_mapping(JournalSuperBlock::default(), Vec::new())
            .expect_err("empty journal mappings are corrupt");

        assert_eq!(error, Ext4Error::corrupted());
        assert_eq!(dev.journal_sequence(), None);
    }

    #[test]
    fn direct_metadata_handle_restores_all_touched_home_blocks_on_error() {
        let first = AbsoluteBN::new(10);
        let second = AbsoluteBN::new(11);
        let first_before = vec![0x11; BLOCK_SIZE];
        let second_before = vec![0x22; BLOCK_SIZE];
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), false);
        dev.write_blocks(&first_before, first, 1, false)
            .expect("write first baseline");
        dev.write_blocks(&second_before, second, 1, false)
            .expect("write second baseline");

        let error = dev
            .with_transaction_handle(2, |dev| {
                dev.write_blocks(&vec![0xaa; BLOCK_SIZE], first, 1, true)?;
                dev.write_blocks(&vec![0xbb; BLOCK_SIZE], second, 1, true)?;
                Err::<(), _>(Ext4Error::io())
            })
            .expect_err("operation failure must abort direct metadata handle");
        assert_eq!(error, Ext4Error::io());

        let mut first_after = vec![0; BLOCK_SIZE];
        let mut second_after = vec![0; BLOCK_SIZE];
        dev.read_blocks(&mut first_after, first, 1)
            .expect("read restored first block");
        dev.read_blocks(&mut second_after, second, 1)
            .expect("read restored second block");
        assert_eq!(first_after, first_before);
        assert_eq!(second_after, second_before);
    }

    #[test]
    fn direct_metadata_handle_credit_overrun_restores_earlier_write() {
        let first = AbsoluteBN::new(10);
        let second = AbsoluteBN::new(11);
        let first_before = vec![0x11; BLOCK_SIZE];
        let second_before = vec![0x22; BLOCK_SIZE];
        let mut dev = Jbd2Dev::initial_jbd2dev(0, MemBlockDev::new(256), false);
        dev.write_blocks(&first_before, first, 1, false)
            .expect("write first baseline");
        dev.write_blocks(&second_before, second, 1, false)
            .expect("write second baseline");

        let error = dev
            .with_transaction_handle(1, |dev| {
                dev.write_blocks(&vec![0xaa; BLOCK_SIZE], first, 1, true)?;
                dev.write_blocks(&vec![0xbb; BLOCK_SIZE], second, 1, true)
            })
            .expect_err("second distinct block must exceed direct handle credits");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::NoSpace);

        let mut first_after = vec![0; BLOCK_SIZE];
        let mut second_after = vec![0; BLOCK_SIZE];
        dev.read_blocks(&mut first_after, first, 1)
            .expect("read restored first block");
        dev.read_blocks(&mut second_after, second, 1)
            .expect("read untouched second block");
        assert_eq!(first_after, first_before);
        assert_eq!(second_after, second_before);
    }
}
