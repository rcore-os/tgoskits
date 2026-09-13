//! Journal ring capacity and bounded metadata/revoke reservations.

use super::*;

impl<B: BlockIo> Jbd2Dev<B> {
    pub(super) fn journal_mapped_blocks(&self) -> Ext4Result<usize> {
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

    pub(super) fn journal_transaction_capacity(&self) -> Ext4Result<usize> {
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

    pub(super) fn journal_revoke_records_per_block(&self) -> Ext4Result<usize> {
        self.ensure_not_aborted("jbd2:capacity_after_abort")?;
        let system = self.system.as_ref().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:capacity_without_state")
        })?;
        system
            .jbd2_super_block
            .revoke_records_per_block(self.inner.block_size() as usize)
    }

    pub(super) fn reserved_buffer_credits(&self) -> Ext4Result<usize> {
        self.reserved_handles
            .iter()
            .try_fold(0usize, |total, handle| {
                total
                    .checked_add(handle.buffer_credits)
                    .ok_or_else(Ext4Error::overflow)
            })
    }

    pub(super) fn reserve_journal_handle(
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

    pub(super) fn remove_journal_reservation(
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

    pub(super) fn journal_maximum_transaction_records(&self) -> Ext4Result<usize> {
        self.ensure_not_aborted("jbd2:capacity_after_abort")?;
        let system = self.system.as_ref().ok_or_else(|| {
            Ext4Error::journal_aborted().with_operation("jbd2:capacity_without_state")
        })?;
        Self::maximum_transaction_records(&system.jbd2_super_block, self.journal_mapped_blocks()?)
    }

    pub(super) fn journal_available_log_records(&self) -> Ext4Result<usize> {
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
            .and_then(|records| records.checked_sub(self.commits.reserved_records()))
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

    pub(super) fn running_transaction_credits(
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

    pub(super) fn transaction_capacity(
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

    pub(super) fn maximum_transaction_records(
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
}
