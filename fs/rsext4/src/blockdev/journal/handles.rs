//! Scoped metadata rollback and reserved transaction ownership.

use super::*;

impl<B: BlockIo> Jbd2Dev<B> {
    /// Runs one metadata operation with a bounded number of queue credits.
    ///
    /// The handle joins this implementation's current in-memory journal queue.
    /// It prevents an automatic commit from splitting the operation and
    /// restores queued metadata images if the operation returns an error. This
    /// is not yet a complete Linux JBD2 handle: the filesystem transaction
    /// owner must also restore its caches and allocation state.
    pub(super) fn with_nested_journal_handle<T>(
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

    pub(super) fn run_active_journal_handle<T>(
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

    pub(super) fn with_journal_handle<T, C>(
        &mut self,
        credits: C,
        operation: impl FnOnce(&mut Self) -> Ext4Result<T>,
    ) -> Ext4Result<T>
    where
        C: Into<TransactionCredits>,
    {
        self.ensure_mutation_admitted()?;
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

    pub(super) fn capture_direct_preimage(&mut self, block_id: AbsoluteBN) -> Ext4Result<()> {
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

    pub(super) fn restore_direct_handle(&mut self, handle: ActiveDirectHandle) -> Ext4Result<()> {
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
}
