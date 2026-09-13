//! Mount-facing writeback lifecycle without OS synchronization primitives.

use super::*;

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Returns the first sticky journal failure, without I/O or state changes.
    /// Embedding caches must reject new dirtying after this failure, including
    /// mutations that would otherwise never reach an inode write immediately.
    pub fn writeback_failure(&self) -> Option<Ext4Error> {
        self.device.journal_abort_cause()
    }

    /// Uses the embedding device's coherent cache as the only ordinary data
    /// cache. Existing private dirty data is written before switching policy.
    pub fn use_shared_device_cache(&mut self) -> Ext4Result<()> {
        self.ensure_mounted("cache:unmounted")?;
        self.writes.ensure_drained()?;
        self.filesystem
            .datablock_cache
            .use_shared_device_cache(&mut self.device)
    }

    /// Seals a fixed durability target for lock-external journal I/O.
    /// Read-only and non-journaled mounts use the explicit synchronous adapter.
    pub fn prepare_sync(&mut self) -> Ext4Result<crate::PreparedCommit<D>>
    where
        D: crate::ForkBlockIo,
    {
        self.stage_sync_for_writeback()?;
        self.device.prepare_commit()
    }

    /// Stages a fixed sync target and closes checkpoint admission in the same
    /// successful seal. Preparation failure never abandons a sealed owner.
    pub fn prepare_sync_for_checkpoint(&mut self) -> Ext4Result<crate::PreparedCommit<D>>
    where
        D: crate::ForkBlockIo,
    {
        self.stage_sync_for_writeback()?;
        self.device.prepare_commit_for_checkpoint()
    }

    fn stage_sync_for_writeback(&mut self) -> Ext4Result<()> {
        self.ensure_writable("sync:prepare")?;
        self.device.ensure_detach_ready()?;
        self.filesystem.stage_sync_metadata(&mut self.device)
    }

    /// Publishes a completed detached commit without issuing device I/O.
    /// If the receipt then needs abort persistence, execute that outside
    /// exclusion and publish the same receipt again.
    pub fn finish_sync(&mut self, receipt: &mut crate::CommitReceipt<D>) -> Ext4Result<()> {
        self.device.finish_commit(receipt)
    }

    /// Returns an independent I/O owner for an unpersisted background abort.
    /// This remains available after mutation has been disabled by that abort.
    /// If persistence was already attempted, returns the original sticky error.
    pub fn prepare_writeback_abort(&mut self) -> Ext4Result<Option<crate::PreparedCommit<D>>>
    where
        D: crate::ForkBlockIo,
    {
        self.device.prepare_abort()
    }

    /// Returns whether this mount durably completed the ticket's fixed target.
    pub fn ticket_is_durable(&self, ticket: &crate::SyncTicket) -> Ext4Result<bool> {
        self.device.ticket_is_durable(ticket)
    }

    /// Enables external journal progress after validating the independent I/O
    /// capability and draining mount-time metadata. Call before publishing the
    /// mount to other tasks; this initialization step may perform synchronous I/O.
    pub fn enable_background_writeback(&mut self) -> Ext4Result<()>
    where
        D: crate::ForkBlockIo,
    {
        self.ensure_writable("writeback:enable")?;
        self.writes.ensure_drained()?;
        self.device.check_detached_io()?;
        self.sync()?;
        self.device.flush()?;
        self.device.enable_background_commits()
    }

    /// Restores synchronous journal cleanup after detached journal owners drain.
    /// Ordinary data owners may remain active: they retain mapping leases, not
    /// journal reservations, and publish into the then-current running state.
    pub fn disable_background_writeback(&mut self) -> Ext4Result<()> {
        self.device.disable_background_commits()
    }

    /// Ends a synchronous journal compatibility section after detached journal
    /// I/O drains. The embedding mount must already own a functioning worker.
    pub fn resume_background_writeback(&mut self) -> Ext4Result<()> {
        self.device.enable_background_commits()
    }

    /// Seals already-published metadata to free capacity for staging the rest.
    /// The caller must retry `prepare_sync` after this intermediate receipt;
    /// this ticket alone does not cover metadata still held in local caches.
    pub fn prepare_writeback_progress(&mut self) -> Ext4Result<crate::PreparedCommit<D>>
    where
        D: crate::ForkBlockIo,
    {
        self.ensure_writable("writeback:progress")?;
        self.device.prepare_commit()
    }

    /// Seals an intermediate metadata prefix and closes checkpoint admission
    /// atomically, allowing staging to resume after reclaiming journal space.
    pub fn prepare_writeback_progress_for_checkpoint(
        &mut self,
    ) -> Ext4Result<crate::PreparedCommit<D>>
    where
        D: crate::ForkBlockIo,
    {
        self.ensure_writable("writeback:progress")?;
        self.device.prepare_commit_for_checkpoint()
    }

    /// Whether checkpoint preparation must be retried before staging more
    /// metadata. This stays true after recoverable endpoint/allocation errors.
    pub fn writeback_checkpoint_pending(&self) -> bool {
        self.device.mutations_paused()
    }

    /// Returns the next checkpoint owner after its preceding commit is durable.
    pub fn prepare_writeback_checkpoint(&mut self) -> Ext4Result<crate::PreparedCommit<D>>
    where
        D: crate::ForkBlockIo,
    {
        self.device.prepare_checkpoint()
    }

    /// Whether the log needs home writeback before another full transaction.
    pub fn writeback_checkpoint_needed(&self) -> Ext4Result<bool> {
        self.device.checkpoint_needed()
    }
}
