//! Final clean publication owns an independent endpoint and the MMP lease.

use super::*;
use crate::{
    SyncTicket,
    ext4::{
        mmp::MmpState,
        sync::{clean_superblock, write_clean_superblock},
    },
    runtime::{Event, JournalEvent, MountEvent},
    superblock::Ext4Superblock,
};

/// Final clean-superblock/MMP I/O after data, journal and checkpoint drain.
/// Dropping this owner does not reopen the mount or publish clean completion.
#[must_use = "execute outside filesystem exclusion and publish the unmount receipt"]
pub struct PreparedUnmount<D: BlockIo> {
    device: Jbd2Dev<D>,
    superblock: Ext4Superblock,
    mmp: MmpState,
    ticket: SyncTicket,
}

impl<D: BlockIo> core::fmt::Debug for PreparedUnmount<D> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PreparedUnmount")
            .field("ticket", &self.ticket)
            .finish_non_exhaustive()
    }
}

/// Actual result of the one-shot final clean publication.
#[derive(Debug)]
#[must_use = "publish even a failed unmount receipt to retain the original error"]
pub struct UnmountReceipt {
    ticket: SyncTicket,
    superblock: Ext4Superblock,
    result: Ext4Result<()>,
}

impl<D: BlockIo> PreparedUnmount<D> {
    /// Writes clean state only after the already-completed checkpoint tail,
    /// then releases MMP ownership. An uncertain release is never retried.
    pub fn execute(mut self) -> UnmountReceipt {
        let result = write_clean_superblock(&mut self.device, &self.superblock)
            .and_then(|()| self.mmp.release_clean(&mut self.device, &self.superblock));
        UnmountReceipt {
            ticket: self.ticket,
            superblock: self.superblock,
            result,
        }
    }
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Seals the terminal unmount owner after external commit/checkpoint
    /// drain. The adapter must stop MMP refresh and exclude new operations.
    /// Returns Busy if any cache, orphan, handle or journal work is undrained.
    pub fn prepare_unmount(&mut self) -> Ext4Result<PreparedUnmount<D>>
    where
        D: crate::ForkBlockIo,
    {
        self.ensure_writable("unmount:prepare")?;
        self.writes.ensure_drained()?;
        if self.filesystem.superblock.s_last_orphan != 0
            || self.filesystem.superblock_dirty
            || self.filesystem.dirty_group_descs.iter().any(|dirty| *dirty)
            || self.filesystem.datablock_cache.stats().dirty_entries != 0
            || self.filesystem.inodetable_cache.stats().dirty_entries != 0
            || self.filesystem.bitmap_cache.stats().dirty_entries != 0
        {
            return Err(Ext4Error::busy().with_operation("unmount:undrained_metadata"));
        }
        let (device, ticket) = self.device.fork_clean_publication()?;
        let superblock = clean_superblock(self.filesystem.superblock);
        self.services
            .observer
            .event(Event::Mount(MountEvent::UnmountStarted));
        self.shutdown_ticket = Some(ticket.clone());
        self.filesystem.mounted = false;
        let mmp = core::mem::take(&mut self.filesystem.mmp);
        Ok(PreparedUnmount {
            device,
            superblock,
            mmp,
            ticket,
        })
    }

    /// Publishes one actual final I/O result without issuing device I/O.
    /// A foreign receipt remains usable by its originating mount.
    pub fn finish_unmount(&mut self, receipt: &UnmountReceipt) -> Ext4Result<()> {
        let expected = self.shutdown_ticket.as_ref().ok_or_else(|| {
            Ext4Error::invalid_input().with_operation("unmount:unexpected_receipt")
        })?;
        if expected.generation() != receipt.ticket.generation()
            || !self.device.ticket_is_durable(&receipt.ticket)?
        {
            return Err(Ext4Error::invalid_input().with_operation("unmount:foreign_receipt"));
        }
        self.shutdown_ticket = None;
        self.unmount_error = receipt.result.err();
        match receipt.result {
            Ok(()) => {
                self.filesystem.superblock = receipt.superblock;
                self.services
                    .observer
                    .event(Event::Journal(JournalEvent::Committed));
                self.services
                    .observer
                    .event(Event::Mount(MountEvent::Unmounted));
            }
            Err(error) => self.filesystem.mmp.mark_failed(error),
        }
        receipt.result
    }
}
