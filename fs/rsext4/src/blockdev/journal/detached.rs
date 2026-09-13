//! Owned journal commit I/O, independent from the mutable filesystem owner.

use alloc::{sync::Arc, vec::Vec};

use super::*;

/// Supplies independent I/O endpoints for the same coherent block device.
///
/// Both endpoints must address the same device and region, share buffered
/// cache coherence, and preserve flush/FUA ordering across endpoints. The
/// endpoint must remain valid until it is dropped, including after unmount.
pub trait ForkBlockIo: BlockIo + Sized {
    /// Returns another endpoint without reading or writing device contents.
    /// Unsupported runtimes must return an explicit capability error.
    fn fork_io(&self) -> Ext4Result<Self>;
}

/// Fixed durability target within one mount; later writes do not extend it.
#[derive(Clone, Debug)]
pub struct SyncTicket {
    mount: Arc<()>,
    generation: u64,
}

impl SyncTicket {
    /// Mount-local sequence used for diagnostics, not an on-disk journal TID.
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

pub(super) struct CommitLedger {
    mount: Arc<()>,
    issued: u64,
    durable: u64,
    pending: Option<PendingCommit>,
    background: Option<Arc<()>>,
    progress_requested: bool,
    checkpoint_requested: bool,
    abort_pending: bool,
}

struct PendingCommit {
    ticket: SyncTicket,
    reserved_records: usize,
    work: JournalWork,
}

impl CommitLedger {
    pub(super) fn new() -> Self {
        Self {
            mount: Arc::new(()),
            issued: 0,
            durable: 0,
            pending: None,
            background: None,
            progress_requested: false,
            checkpoint_requested: false,
            abort_pending: false,
        }
    }

    pub(super) fn ensure_idle(&self) -> Ext4Result<()> {
        if self.pending.is_some() {
            Err(Ext4Error::busy().with_operation("jbd2:detached_commit_pending"))
        } else {
            Ok(())
        }
    }

    pub(super) fn reserved_records(&self) -> usize {
        self.pending
            .as_ref()
            .map_or(0, |pending| pending.reserved_records)
    }

    pub(super) fn defer_abort_persistence(&mut self) -> bool {
        if self.background.is_some() || self.pending.is_some() {
            self.abort_pending = true;
            true
        } else {
            false
        }
    }

    fn next_ticket(&self) -> Ext4Result<SyncTicket> {
        Ok(SyncTicket {
            mount: Arc::clone(&self.mount),
            generation: self.issued.checked_add(1).ok_or_else(Ext4Error::overflow)?,
        })
    }

    pub(super) fn request_external_progress(&mut self) -> Ext4Result<()> {
        if self.background.is_some() {
            self.progress_requested = true;
            Err(Ext4Error::journal_progress())
        } else {
            Ok(())
        }
    }
}

/// Home reads are valid only within one background session and commit epoch.
/// Holding the session Arc prevents identity reuse while a read is in flight.
#[derive(Debug)]
pub(crate) struct MetadataReadVersion {
    session: Arc<()>,
    issued: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JournalWork {
    Flush,
    Commit(Jbd2CommitTimestamp),
    Checkpoint,
    Abort(Ext4Error),
}

enum MutationAdmission {
    Open,
    Checkpoint,
}

/// Sealed transaction plus a coherent, independently owned I/O endpoint.
///
/// Dropping this token does not publish success: the mount remains pending
/// and retains the sealed metadata images. Execute it outside filesystem locks
/// and return its receipt to the originating mount before shutdown.
#[must_use = "execute the commit and publish its receipt to the originating filesystem"]
pub struct PreparedCommit<D: BlockIo> {
    ticket: SyncTicket,
    device: BlockDev<D>,
    journal: Option<JBD2DEVSYSTEM>,
    mapping: Vec<AbsoluteBN>,
    work: JournalWork,
}

impl<D: BlockIo> core::fmt::Debug for PreparedCommit<D> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PreparedCommit")
            .field("ticket", &self.ticket)
            .field("journaled", &self.journal.is_some())
            .finish_non_exhaustive()
    }
}

/// Actual I/O result, retaining the journal state until the mount accepts it.
#[must_use = "publish the receipt even when commit I/O failed"]
pub struct CommitReceipt<D: BlockIo> {
    ticket: SyncTicket,
    journal: Option<JBD2DEVSYSTEM>,
    result: Ext4Result<()>,
    abort_persistence_error: Option<Ext4Error>,
    work: JournalWork,
    device: BlockDev<D>,
    mapping: Vec<AbsoluteBN>,
    abort_required: bool,
    abort_attempted: bool,
}

impl<D: BlockIo> core::fmt::Debug for CommitReceipt<D> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CommitReceipt")
            .field("ticket", &self.ticket)
            .field("result", &self.result)
            .field("abort_persistence_error", &self.abort_persistence_error)
            .finish_non_exhaustive()
    }
}

impl<D: BlockIo> CommitReceipt<D> {
    /// Whether publication observed an abort that this I/O owner must record.
    pub fn needs_abort_persistence(&self) -> bool {
        self.abort_required && !self.abort_attempted
    }

    /// Records an abort after detached I/O has ended, outside filesystem locks.
    /// Call `finish_commit` again afterwards to publish the persistence result.
    pub fn persist_abort(&mut self) {
        if !self.needs_abort_persistence() {
            return;
        }
        self.abort_persistence_error = self.journal.as_mut().and_then(|journal| {
            journal
                .record_abort_with_mapping(&mut self.device, &self.mapping)
                .err()
        });
        self.abort_attempted = true;
    }
}

impl<D: BlockIo> PreparedCommit<D> {
    /// Returns the exact target represented by this sealed batch.
    pub fn ticket(&self) -> SyncTicket {
        self.ticket.clone()
    }

    /// Executes ordered-data and journal I/O without borrowing the filesystem.
    /// Even failure returns a receipt so every waiter can be completed.
    pub fn execute(mut self) -> CommitReceipt<D> {
        let result = if let JournalWork::Abort(cause) = self.work {
            Err(cause)
        } else {
            self.device.flush().and_then(|()| match &mut self.journal {
                Some(journal) => match self.work {
                    JournalWork::Flush => Ok(()),
                    JournalWork::Commit(timestamp) => journal
                        .write_committing_transaction(&mut self.device, &self.mapping, timestamp)
                        .map(|_| ()),
                    JournalWork::Abort(cause) => Err(cause),
                    JournalWork::Checkpoint => journal
                        .checkpoint_transactions_with_mapping(
                            &mut self.device,
                            &self.mapping,
                            usize::MAX,
                        )
                        .map(|_| ()),
                },
                None => Ok(()),
            })
        };
        let abort_persistence_error = if result.is_err() {
            self.journal.as_mut().and_then(|journal| {
                journal
                    .record_abort_with_mapping(&mut self.device, &self.mapping)
                    .err()
            })
        } else {
            None
        };
        CommitReceipt {
            ticket: self.ticket,
            journal: self.journal,
            result,
            abort_persistence_error,
            work: self.work,
            device: self.device,
            mapping: self.mapping,
            abort_required: result.is_err(),
            abort_attempted: result.is_err(),
        }
    }
}

impl<B: ForkBlockIo> Jbd2Dev<B> {
    pub(crate) fn fork_file_data_endpoint(
        &self,
    ) -> Ext4Result<crate::blockdev::FileDataEndpoint<B>> {
        crate::blockdev::FileDataEndpoint::new(
            self.inner._device().fork_io()?,
            self.inner.block_size() as usize,
        )
    }

    pub(crate) fn fork_read_endpoint(&self) -> Ext4Result<B> {
        self.inner._device().fork_io()
    }

    /// Detaches a latched background abort without committing failed metadata.
    /// No clock or data flush is needed to persist the journal error marker.
    /// Once persistence was attempted, subsequent sync attempts return the
    /// original failure instead of replacing it with a generic aborted error.
    pub fn prepare_abort(&mut self) -> Ext4Result<Option<PreparedCommit<B>>> {
        if !self.commits.abort_pending {
            return self.journal_abort_cause().map_or(Ok(None), Err);
        }
        self.commits.ensure_idle()?;
        self.ensure_no_active_edits()?;
        let cause = self
            .journal_abort_cause()
            .ok_or_else(Ext4Error::corrupted)?;
        let ticket = self.commits.next_ticket()?;
        let mut device = BlockDev::new(self.inner._device().fork_io()?);
        device.set_filesystem_block_size(self.inner.block_size() as usize)?;
        let mut mapping = Vec::new();
        mapping
            .try_reserve_exact(self.journal_blocks.len())
            .map_err(|_| Ext4Error::no_memory())?;
        mapping.extend_from_slice(&self.journal_blocks);
        let system = self
            .system
            .as_ref()
            .ok_or_else(Ext4Error::journal_aborted)?;
        let journal = Some(JBD2DEVSYSTEM {
            jbd2_super_block: system.jbd2_super_block,
            start_block: system.start_block,
            max_len: system.max_len,
            head: system.head,
            sequence: system.sequence,
            running_transaction: Jbd2RunningTransaction::default(),
            committing_transaction: None,
            checkpoint_transactions: Vec::new(),
            used_log_records: system.used_log_records,
        });
        let work = JournalWork::Abort(cause);
        self.commits.issued = ticket.generation;
        self.commits.pending = Some(PendingCommit {
            ticket: ticket.clone(),
            reserved_records: 0,
            work,
        });
        Ok(Some(PreparedCommit {
            ticket,
            device,
            journal,
            mapping,
            work,
        }))
    }

    pub(crate) fn fork_clean_publication(&self) -> Ext4Result<(Self, SyncTicket)> {
        self.ensure_clean_publication_ready()?;
        let timestamp = (self.clock)(self.inner._device())?;
        let mut device = Self::with_clock_callback(
            0,
            self.inner._device().fork_io()?,
            false,
            alloc::boxed::Box::new(move |_| Ok(timestamp)),
        );
        device.set_filesystem_block_size(self.block_size() as usize)?;
        let ticket = SyncTicket {
            mount: Arc::clone(&self.commits.mount),
            generation: self.commits.durable,
        };
        Ok((device, ticket))
    }
    /// Validates the detached-I/O capability before the adapter starts its
    /// worker. No journal owner is transferred and no I/O is issued.
    pub(crate) fn check_detached_io(&self) -> Ext4Result<()> {
        self.ensure_detach_ready()?;
        if !self.journal_use {
            return Err(Ext4Error::unsupported_capability(
                "jbd2:background_without_journal",
            ));
        }
        drop(self.inner._device().fork_io()?);
        Ok(())
    }

    /// Detaches checkpoint I/O only after admission has been closed and the
    /// last running transaction has committed. Reads remain available, but
    /// free/reuse must wait until the checkpoint receipt is published.
    pub fn prepare_checkpoint(&mut self) -> Ext4Result<PreparedCommit<B>> {
        self.ensure_not_aborted("jbd2:checkpoint_after_abort")?;
        self.commits.ensure_idle()?;
        self.ensure_no_active_edits()?;
        if !self.commits.checkpoint_requested || self.has_running_updates() {
            return Err(Ext4Error::busy().with_operation("jbd2:checkpoint_not_quiescent"));
        }
        let ticket = self.commits.next_ticket()?;
        let mut device = BlockDev::new(self.inner._device().fork_io()?);
        device.set_filesystem_block_size(self.inner.block_size() as usize)?;
        let system = self
            .system
            .as_ref()
            .ok_or_else(Ext4Error::journal_aborted)?;
        let mut mapping = Vec::new();
        mapping
            .try_reserve_exact(self.journal_blocks.len())
            .map_err(|_| Ext4Error::no_memory())?;
        mapping.extend_from_slice(&self.journal_blocks);
        let mut checkpoints = Vec::new();
        checkpoints
            .try_reserve_exact(system.checkpoint_transactions.len())
            .map_err(|_| Ext4Error::no_memory())?;
        checkpoints.extend(system.checkpoint_transactions.iter().cloned());
        let journal = JBD2DEVSYSTEM {
            jbd2_super_block: system.jbd2_super_block,
            start_block: system.start_block,
            max_len: system.max_len,
            head: system.head,
            sequence: system.sequence,
            running_transaction: Jbd2RunningTransaction::default(),
            committing_transaction: None,
            checkpoint_transactions: checkpoints,
            used_log_records: system.used_log_records,
        };
        self.commits.issued = ticket.generation;
        self.commits.pending = Some(PendingCommit {
            ticket: ticket.clone(),
            reserved_records: 0,
            work: JournalWork::Checkpoint,
        });
        Ok(PreparedCommit {
            ticket,
            device,
            journal: Some(journal),
            mapping,
            work: JournalWork::Checkpoint,
        })
    }

    /// Seals complete metadata operations and leaves a fresh running owner.
    /// This method performs no commit I/O. Capacity is reserved before sealing;
    /// a busy/no-space result requires lock-external progress before retrying.
    pub fn prepare_commit(&mut self) -> Ext4Result<PreparedCommit<B>> {
        self.prepare_commit_with_admission(MutationAdmission::Open)
    }

    /// Seals the running transaction and closes mutation admission atomically.
    /// All fallible preparation precedes the state transition, so a failed
    /// preparation leaves admission open and does not strand an I/O owner.
    pub fn prepare_commit_for_checkpoint(&mut self) -> Ext4Result<PreparedCommit<B>> {
        self.prepare_commit_with_admission(MutationAdmission::Checkpoint)
    }

    fn prepare_commit_with_admission(
        &mut self,
        admission: MutationAdmission,
    ) -> Ext4Result<PreparedCommit<B>> {
        self.ensure_not_aborted("jbd2:prepare_after_abort")?;
        self.commits.ensure_idle()?;
        if !self.journal_use {
            return Err(Ext4Error::unsupported_capability(
                "jbd2:detached_without_journal",
            ));
        }
        self.ensure_no_active_edits()?;
        let ticket = self.commits.next_ticket()?;
        let timestamp = Jbd2CommitTimestamp::try_from((self.clock)(self.inner._device())?)?;
        let mut device = BlockDev::new(self.inner._device().fork_io()?);
        device.set_filesystem_block_size(self.inner.block_size() as usize)?;
        let mut mapping = Vec::new();
        mapping
            .try_reserve_exact(self.journal_blocks.len())
            .map_err(|_| Ext4Error::no_memory())?;
        mapping.extend_from_slice(&self.journal_blocks);
        let maximum_records = self.journal_maximum_transaction_records()?;
        if self.has_running_updates() && self.journal_available_log_records()? < maximum_records {
            return Err(Ext4Error::no_space().with_operation("jbd2:prepare_log_space"));
        }
        let system = self.system.as_mut().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:prepare_without_state")
        })?;
        // Reserve the publication slot before transferring the running owner.
        system
            .checkpoint_transactions
            .try_reserve(1)
            .map_err(|_| Ext4Error::no_memory())?;
        let committing = system.start_committing_transaction()?;
        let journal = Some(JBD2DEVSYSTEM {
            jbd2_super_block: system.jbd2_super_block,
            start_block: system.start_block,
            max_len: system.max_len,
            head: system.head,
            sequence: system.sequence,
            running_transaction: Jbd2RunningTransaction::default(),
            committing_transaction: system.committing_transaction.clone(),
            checkpoint_transactions: Vec::new(),
            used_log_records: system.used_log_records,
        });
        let work = if committing {
            JournalWork::Commit(timestamp)
        } else {
            JournalWork::Flush
        };
        self.commits.issued = ticket.generation;
        self.commits.pending = Some(PendingCommit {
            ticket: ticket.clone(),
            reserved_records: if committing { maximum_records } else { 0 },
            work,
        });
        if matches!(admission, MutationAdmission::Checkpoint) {
            self.commits.checkpoint_requested = true;
        }
        self.inner.discard_held();
        Ok(PreparedCommit {
            ticket,
            device,
            journal,
            mapping,
            work,
        })
    }
}

impl<B: BlockIo> Jbd2Dev<B> {
    /// A coherent endpoint can replace ordinary data while this owner retains
    /// a read-only held image. Mutable unpublished edits cannot be discarded.
    pub(crate) fn discard_detached_data_image(&mut self) -> Ext4Result<()> {
        if self.inner.has_unpublished_edit() {
            return Err(Ext4Error::busy().with_operation("write:unpublished_block_edit"));
        }
        self.inner.discard_held();
        Ok(())
    }

    pub(crate) fn metadata_read_version(&self) -> Ext4Result<Option<MetadataReadVersion>> {
        self.ensure_detach_ready()?;
        self.ensure_mutation_admitted()?;
        Ok(self
            .commits
            .background
            .as_ref()
            .map(|session| MetadataReadVersion {
                session: session.clone(),
                issued: self.commits.issued,
            }))
    }

    pub(crate) fn validate_metadata_read(&self, version: &MetadataReadVersion) -> Ext4Result<()> {
        self.ensure_detach_ready()?;
        self.ensure_mutation_admitted()?;
        if !self
            .commits
            .background
            .as_ref()
            .is_some_and(|session| Arc::ptr_eq(session, &version.session))
        {
            return Err(
                Ext4Error::invalid_input().with_operation("inode_table:foreign_read_session")
            );
        }
        if self.commits.issued != version.issued {
            return Err(Ext4Error::busy().with_operation("inode_table:stale_read_epoch"));
        }
        Ok(())
    }

    pub(crate) fn read_mount_identity(&self) -> Arc<()> {
        Arc::clone(&self.commits.mount)
    }

    pub(crate) fn owns_read_mount(&self, mount: &Arc<()>) -> bool {
        Arc::ptr_eq(&self.commits.mount, mount)
    }

    pub(crate) fn ensure_clean_publication_ready(&self) -> Ext4Result<()> {
        self.ensure_not_aborted("unmount:journal_aborted")?;
        self.ensure_journal_state_reinstallable()?;
        self.ensure_no_active_edits()
    }

    pub(crate) fn ensure_detach_ready(&self) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:prepare_after_abort")?;
        self.commits.ensure_idle()?;
        self.ensure_no_active_edits()
    }

    fn ensure_no_active_edits(&self) -> Ext4Result<()> {
        if self.active_handle.is_some()
            || self.active_direct_handle.is_some()
            || !self.reserved_handles.is_empty()
            || self.inner.has_unpublished_edit()
        {
            Err(Ext4Error::busy().with_operation("jbd2:prepare_with_active_edit"))
        } else {
            Ok(())
        }
    }

    /// Rejects mutation before it can alter caches, allocator state or data.
    /// The adapter waits outside filesystem exclusion while checkpoint owns
    /// admission. MMP refresh uses a separate, reserved on-disk block.
    pub(crate) fn ensure_mutation_admitted(&self) -> Ext4Result<()> {
        // Keep the established mutation error contract. The receipt and
        // ticket query retain the original I/O error for synchronization.
        self.ensure_not_aborted("jbd2:mutation_after_abort")?;
        if self.mutations_paused() {
            Err(Ext4Error::journal_progress())
        } else {
            Ok(())
        }
    }

    /// Requests lock-external progress instead of inline commits at capacity.
    pub fn enable_background_commits(&mut self) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:enable_background")?;
        self.commits.ensure_idle()?;
        if !self.journal_use {
            return Err(Ext4Error::unsupported_capability(
                "jbd2:background_without_journal",
            ));
        }
        self.commits.background.get_or_insert_with(|| Arc::new(()));
        Ok(())
    }

    /// Returns to synchronous cleanup only after all detached I/O has drained.
    pub fn disable_background_commits(&mut self) -> Ext4Result<()> {
        self.commits.ensure_idle()?;
        if self.mutations_paused() {
            return Err(Ext4Error::busy().with_operation("jbd2:disable_during_checkpoint"));
        }
        self.commits.background = None;
        Ok(())
    }

    /// Stops new mutations before draining the last transaction for checkpoint.
    pub fn begin_checkpoint(&mut self) -> Ext4Result<()> {
        self.ensure_not_aborted("jbd2:begin_checkpoint")?;
        self.ensure_no_active_edits()?;
        if !self.journal_use || self.has_running_updates() {
            return Err(Ext4Error::busy().with_operation("jbd2:checkpoint_before_seal"));
        }
        self.commits.checkpoint_requested = true;
        Ok(())
    }

    /// Whether the caller must release filesystem exclusion before waiting.
    pub fn background_progress_requested(&self) -> bool {
        self.commits.progress_requested
    }

    /// Checkpoint serializes free/reuse without holding exclusion across I/O.
    pub fn mutations_paused(&self) -> bool {
        self.commits.checkpoint_requested
    }

    /// Whether complete operations have left journal metadata to commit.
    pub fn has_running_updates(&self) -> bool {
        self.system.as_ref().is_some_and(|system| {
            !system.running_transaction.updates.is_empty()
                || !system.running_transaction.revoked_blocks.is_empty()
        })
    }

    /// Requests checkpoint while there is still room to seal a full transaction.
    pub fn checkpoint_needed(&self) -> Ext4Result<bool> {
        if !self.journal_use {
            return Ok(false);
        }
        let maximum = self.journal_maximum_transaction_records()?;
        Ok(self.journal_available_log_records()? < maximum.saturating_mul(2))
    }

    /// Publishes one actual commit result under the short filesystem lock.
    /// Failure is sticky and retains the primary owner's sealed images.
    /// An invalid receipt remains owned by the caller and can still be
    /// published to its origin. Publishing an accepted receipt twice fails.
    /// If an abort raced with successful I/O, the receipt remains pending:
    /// check `needs_abort_persistence`, call `persist_abort` outside filesystem
    /// exclusion, and publish the same receipt again, even on persistence error.
    pub fn finish_commit(&mut self, receipt: &mut CommitReceipt<B>) -> Ext4Result<()> {
        let pending = self.commits.pending.as_ref().ok_or_else(|| {
            Ext4Error::invalid_input().with_operation("jbd2:unexpected_commit_receipt")
        })?;
        if !Arc::ptr_eq(&receipt.ticket.mount, &self.commits.mount)
            || receipt.ticket.generation != pending.ticket.generation
            || receipt.work != pending.work
        {
            return Err(Ext4Error::invalid_input().with_operation("jbd2:foreign_commit_receipt"));
        }
        if let Some(cause) = self.journal_abort_cause() {
            receipt.abort_required = true;
            if receipt.needs_abort_persistence() {
                // Retain pending ownership until the same receipt completes
                // its lock-external abort write. No other header writer may run.
                return Err(cause);
            }
            if let Some(state) = self.abort_state.as_mut() {
                state.persistence_error = receipt.abort_persistence_error;
            }
            self.commits.pending = None;
            self.commits.abort_pending = false;
            return Err(cause);
        }
        if let Err(cause) = receipt.result {
            if self.abort_state.is_none() {
                self.abort_state = Some(JournalAbortState {
                    cause,
                    replay_failure: None,
                    persistence_error: receipt.abort_persistence_error,
                });
            }
            self.commits.pending = None;
            return Err(cause);
        }
        // Validate every fallible publication precondition before moving any
        // journal owner or changing ring accounting.
        if let Some(committed) = receipt.journal.as_ref()
            && (self.system.is_none()
                || committed.committing_transaction.is_some()
                || match receipt.work {
                    JournalWork::Flush => !committed.checkpoint_transactions.is_empty(),
                    JournalWork::Commit(_) => committed.checkpoint_transactions.len() != 1,
                    JournalWork::Checkpoint => !committed.checkpoint_transactions.is_empty(),
                    JournalWork::Abort(_) => true,
                })
        {
            return Err(Ext4Error::corrupted().with_operation("jbd2:invalid_commit_receipt"));
        }
        if let Some(mut committed) = receipt.journal.take() {
            let system = self
                .system
                .as_mut()
                .ok_or_else(Ext4Error::journal_aborted)?;
            // A detached worker never owns or replaces the new running state.
            system.jbd2_super_block = committed.jbd2_super_block;
            system.head = committed.head;
            system.sequence = committed.sequence;
            system.used_log_records = committed.used_log_records;
            match receipt.work {
                JournalWork::Flush => {}
                JournalWork::Commit(_) => {
                    system
                        .checkpoint_transactions
                        .append(&mut committed.checkpoint_transactions);
                }
                JournalWork::Checkpoint => {
                    system.checkpoint_transactions.clear();
                    self.commits.checkpoint_requested = false;
                }
                JournalWork::Abort(_) => unreachable!("abort receipts cannot publish success"),
            }
            system.committing_transaction = None;
        }
        self.commits.durable = receipt.ticket.generation;
        self.commits.pending = None;
        self.commits.progress_requested = false;
        // Another task may have read an old home image during commit I/O.
        self.inner.discard_held();
        Ok(())
    }

    /// Observes a fixed mount-local durability target without performing I/O.
    pub fn ticket_is_durable(&self, ticket: &SyncTicket) -> Ext4Result<bool> {
        if !Arc::ptr_eq(&ticket.mount, &self.commits.mount) {
            return Err(Ext4Error::invalid_input().with_operation("jbd2:foreign_sync_ticket"));
        }
        if let Some(cause) = self.journal_abort_cause() {
            return Err(cause);
        }
        Ok(self.commits.durable >= ticket.generation)
    }
}

#[cfg(test)]
mod tests {
    use crate::Ext4ErrorKind;

    #[test]
    fn exhausted_commit_sequence_cannot_revalidate_an_old_home_read() {
        let mut journal = super::super::detached_tests::journal();
        journal.enable_background_commits().unwrap();
        journal.commits.issued = u64::MAX - 1;
        let old = journal.metadata_read_version().unwrap().unwrap();
        let mut receipt = journal.prepare_commit().unwrap().execute();
        journal.finish_commit(&mut receipt).unwrap();
        assert_eq!(journal.commits.issued, u64::MAX);
        assert_eq!(
            journal.validate_metadata_read(&old).unwrap_err().kind(),
            Ext4ErrorKind::Busy
        );

        assert_eq!(
            journal.prepare_commit().unwrap_err().kind(),
            Ext4ErrorKind::Overflow
        );
        assert_eq!(journal.commits.issued, u64::MAX);
        assert_eq!(
            journal.validate_metadata_read(&old).unwrap_err().kind(),
            Ext4ErrorKind::Busy
        );
        assert!(journal.commits.pending.is_none());
    }
}
