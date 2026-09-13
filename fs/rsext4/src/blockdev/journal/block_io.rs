//! Journal-visible block reads, metadata images and direct data I/O.

use super::*;

impl<B: BlockIo> Jbd2Dev<B> {
    /// Writes the current internal block buffer.
    pub(crate) fn write_block(
        &mut self,
        block_id: AbsoluteBN,
        is_metadata: bool,
    ) -> Ext4Result<()> {
        self.ensure_mutation_admitted()?;
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
        self.ensure_mutation_admitted()?;
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
        self.ensure_mutation_admitted()?;
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

    /// Borrows the newest journal-owned image without issuing device I/O.
    /// Revoke visibility is shared with normal reads; callers must snapshot
    /// the bytes before releasing the mounted owner's exclusion.
    pub(crate) fn visible_block_image(&self, block: AbsoluteBN) -> Option<&[u8]> {
        if !self.journal_use {
            return None;
        }
        let system = self.system.as_ref()?;
        system
            .running_transaction
            .updates
            .iter()
            .find(|update| update.0 == block)
            .or_else(|| Self::visible_committed_update(system, block))
            .map(|update| &update.1[..])
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

        // Inode-table read/modify/write requests already have the complete
        // newest image in running, committing or checkpoint ownership. Keep
        // the existing multi-block and oversized-buffer I/O boundary unchanged.
        if count == 1
            && buf.len() == block_size
            && let Some(image) = self.visible_block_image(block_id)
        {
            self.inner.read_window(buf.len(), block_id, count)?;
            if image.len() != block_size {
                return Err(Ext4Error::corrupted().with_operation("jbd2:update_block_size"));
            }
            buf.copy_from_slice(image);
            return Ok(());
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
        self.ensure_mutation_admitted()?;
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
    pub(super) fn enqueue_journal_update(
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

    pub(super) fn clone_updates(queue: &[Jbd2Update]) -> Vec<Jbd2Update> {
        queue
            .iter()
            .map(|update| Jbd2Update(update.0, update.1.to_vec().into_boxed_slice()))
            .collect()
    }

    pub(super) fn visible_committed_update(
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
}
