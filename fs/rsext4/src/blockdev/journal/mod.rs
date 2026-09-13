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

mod block_io;
mod credits;
mod detached;
mod handles;
mod sync;
use detached::CommitLedger;
pub(crate) use detached::MetadataReadVersion;
pub use detached::{CommitReceipt, ForkBlockIo, PreparedCommit, SyncTicket};

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
    commits: CommitLedger,
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
        Self::transaction_capacity(super_block, self.inner.block_size() as usize, mapped_blocks)?;
        Ok(())
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
            commits: CommitLedger::new(),
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
        self.commits.ensure_idle()?;
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

    /// Returns whether this journal records an error from a previous mount.
    pub(crate) fn has_recorded_journal_error(&self) -> bool {
        self.system
            .as_ref()
            .is_some_and(|system| system.jbd2_super_block.s_errno != 0)
    }

    /// Clears a previous mount's durable journal error after ext4 has recorded it.
    pub(crate) fn clear_recorded_journal_error(&mut self) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:clear_error_after_abort")?;
        self.ensure_journal_state_reinstallable()?;
        let Some(system) = self.system.as_mut() else {
            return Err(Ext4Error::corrupted().with_operation("jbd2:clear_error_without_state"));
        };
        if system.jbd2_super_block.s_errno == 0 {
            return Ok(());
        }
        system.clear_recorded_error_with_mapping(&mut self.inner, &self.journal_blocks)
    }
}

impl<B: BlockIo> Clock for Jbd2Dev<B> {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        (self.clock)(self.inner._device())
    }
}

#[cfg(test)]
mod detached_tests;

#[cfg(test)]
mod tests;
