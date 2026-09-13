//! Synchronous commit/checkpoint compatibility and sticky abort state.

use super::*;

impl<B: BlockIo> Jbd2Dev<B> {
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

    pub(crate) fn journal_abort_cause(&self) -> Option<Ext4Error> {
        self.abort_state.as_ref().map(|state| state.cause)
    }

    pub(super) fn ensure_not_aborted(&self, operation: &'static str) -> Ext4Result<()> {
        if self.journal_abort_cause().is_some() {
            Err(Ext4Error::journal_aborted().with_operation(operation))
        } else {
            Ok(())
        }
    }

    pub(super) fn ensure_journal_state_reinstallable(&self) -> Ext4Result<()> {
        self.commits.ensure_idle()?;
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

    pub(super) fn abort_journal(&mut self, cause: Ext4Error) {
        if self.journal_abort_cause().is_some() {
            return;
        }
        self.abort_state = Some(JournalAbortState {
            cause,
            replay_failure: None,
            persistence_error: None,
        });

        // A detached owner may still advance the on-disk journal head. Only
        // that receipt's endpoint may persist this abort after its I/O ends;
        // writing our older header here could erase its recovery boundary.
        if self.commits.defer_abort_persistence() {
            return;
        }

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

    pub(super) fn commit_pending_transaction(&mut self) -> Ext4Result<bool> {
        self.ensure_not_aborted("jbd2:commit_after_abort")?;
        if self.active_handle.is_some() || self.active_direct_handle.is_some() {
            return Err(Ext4Error::busy().with_operation("jbd2:commit_with_active_handle"));
        }
        if self.inner.has_unpublished_edit() {
            return Err(Ext4Error::busy().with_operation("jbd2:commit_with_unfinished_block_edit"));
        }
        // Restarting an empty running owner needs no I/O, even while the
        // previous sealed transaction is still committing on another endpoint.
        if self.system.is_some() && !self.has_running_updates() {
            return Ok(false);
        }
        self.commits.request_external_progress()?;
        self.commits.ensure_idle()?;
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

    pub(super) fn checkpoint_transactions(&mut self, max_transactions: usize) -> Ext4Result<bool> {
        self.ensure_not_aborted("jbd2:checkpoint_after_abort")?;
        self.commits.request_external_progress()?;
        self.commits.ensure_idle()?;
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
    pub(super) fn checkpoint_pending_transactions(&mut self) -> Ext4Result<bool> {
        self.checkpoint_transactions(1)
    }

    pub(super) fn checkpoint_all_pending_transactions(&mut self) -> Ext4Result<bool> {
        self.checkpoint_transactions(usize::MAX)
    }

    pub(super) fn checkpoint_until_log_records(
        &mut self,
        required_records: usize,
    ) -> Ext4Result<()> {
        let mut available_records = self.journal_available_log_records()?;
        if available_records < required_records {
            // An in-flight commit may own all reclaimable records. The
            // adapter must publish it before selecting checkpoint victims.
            self.commits.request_external_progress()?;
        }
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

    pub(super) fn reserve_maximum_transaction_log_space(&mut self) -> Ext4Result<()> {
        let required_records = self.journal_maximum_transaction_records()?;
        self.checkpoint_until_log_records(required_records)
    }
}
