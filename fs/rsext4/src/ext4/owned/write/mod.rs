//! Independent ordinary-file data writes with mount-owned mapping leases.

use alloc::sync::Arc;

use super::*;
use crate::{
    ForkBlockIo,
    blockdev::FileDataEndpoint,
    file::{CompletedFileWrite, PreparedFileWrite},
};

mod owners;

use owners::InodeWriteIdentity;
pub(super) use owners::InodeWriteOwners;

/// Prepared file data whose mapping cannot be truncated or reclaimed until
/// the mount accepts its completion. The adapter additionally retains inode
/// content exclusion and mount admission through metadata publication.
///
/// Dropping this owner deliberately leaves the mapping lease pending. Use
/// `cancel` and publish its receipt when abandoning a write before I/O.
#[must_use = "execute or cancel, then publish the receipt on the originating mount"]
pub struct PreparedInodeWrite<'a, D: BlockIo> {
    mount: Arc<()>,
    identity: InodeWriteIdentity,
    device: FileDataEndpoint<D>,
    plan: PreparedFileWrite,
    input: &'a [u8],
}

/// Completed data I/O, retained across retryable metadata publication.
/// Foreign and already consumed receipts cannot publish inode state.
#[must_use = "publish even a failed data write so its mapping lease is released"]
pub struct CompletedInodeWrite {
    mount: Arc<()>,
    identity: InodeWriteIdentity,
    completed: Option<CompletedFileWrite>,
}

impl CompletedInodeWrite {
    /// Whether the mount still owns a mapping lease for this completion.
    pub fn needs_publication(&self) -> bool {
        self.completed.is_some()
    }
}

impl<D: BlockIo> core::fmt::Debug for PreparedInodeWrite<'_, D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PreparedInodeWrite")
            .field("inode", &self.identity.inode)
            .field("bytes", &self.input.len())
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for CompletedInodeWrite {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CompletedInodeWrite")
            .field("inode", &self.identity.inode)
            .field(
                "result",
                &self.completed.as_ref().map(CompletedFileWrite::io_result),
            )
            .finish_non_exhaustive()
    }
}

impl<D: BlockIo> PreparedInodeWrite<'_, D> {
    /// Executes all data I/O without borrowing mounted filesystem state.
    /// Errors return a receipt; no unwritten mapping is converted here.
    pub fn execute(mut self) -> CompletedInodeWrite {
        let completed = self.plan.execute(&mut self.device, self.input);
        CompletedInodeWrite {
            mount: self.mount,
            identity: self.identity,
            completed: Some(completed),
        }
    }

    /// Cancels before any data I/O. Publish this error receipt to release the
    /// mapping lease; already allocated unwritten extents remain reachable.
    pub fn cancel(self) -> CompletedInodeWrite {
        CompletedInodeWrite {
            mount: self.mount,
            identity: self.identity,
            completed: Some(self.plan.cancel()),
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
    /// Prepares all ordinary extent-file writes, including growth and partial
    /// blocks. `None` selects serialized compatibility for empty/non-regular/
    /// legacy writes, private-cache mode, or unsupported endpoint capability.
    /// Those decisions occur before allocation; I/O/allocation failures never
    /// choose fallback. The input borrow remains immutable until I/O finishes.
    ///
    /// # Errors
    /// Returns mount, mapping, geometry, allocation and journal errors. Journal
    /// pressure before preparation succeeds may be retried after progress.
    pub fn prepare_inode_write<'a>(
        &mut self,
        number: InodeNumber,
        offset: u64,
        input: &'a [u8],
    ) -> Ext4Result<Option<PreparedInodeWrite<'a, D>>>
    where
        D: ForkBlockIo,
    {
        self.ensure_writable("inode:prepare_write")?;
        self.writes.ensure_inode_idle(number)?;
        if input.is_empty() || !self.filesystem.datablock_cache.uses_shared_device_cache() {
            return Ok(None);
        }
        let inode = self.filesystem.get_inode_by_num(&mut self.device, number)?;
        if !inode.uses_extents() || inode.i_mode & Ext4Inode::S_IFMT != Ext4Inode::S_IFREG {
            return Ok(None);
        }
        let end = offset
            .checked_add(u64::try_from(input.len()).map_err(|_| Ext4Error::overflow())?)
            .ok_or_else(Ext4Error::file_too_large)?;
        let device = match self.device.fork_file_data_endpoint() {
            Ok(device) => device,
            Err(error) if error.kind() == Ext4ErrorKind::UnsupportedCapability => return Ok(None),
            Err(error) => return Err(error),
        };
        let plan = PreparedFileWrite::prepare(
            &mut self.device,
            &mut self.filesystem,
            number,
            offset..end,
        )?;
        let identity = self.writes.register(number)?;
        Ok(Some(PreparedInodeWrite {
            mount: self.device.read_mount_identity(),
            identity,
            device,
            plan,
            input,
        }))
    }

    /// Publishes completed data and then releases its mapping lease. Only a
    /// journal-progress result retains the receipt for retry without data I/O.
    /// A failed I/O receipt preserves its original cause and never initializes
    /// extents. Namespace link/orphan changes are merged from the current inode.
    ///
    /// # Errors
    /// Returns the data/metadata failure or rejects a foreign/consumed receipt.
    /// Dropping an unaccepted receipt keeps its lease pending, not successful.
    pub fn finish_inode_write(&mut self, receipt: &mut CompletedInodeWrite) -> Ext4Result<()> {
        if !self.device.owns_read_mount(&receipt.mount) {
            return Err(Ext4Error::invalid_input().with_operation("inode:foreign_write_receipt"));
        }
        self.writes.validate(&receipt.identity)?;
        let completed = receipt.completed.as_mut().ok_or_else(|| {
            Ext4Error::invalid_input().with_operation("inode:consumed_write_receipt")
        })?;
        let result = completed.io_result().and_then(|()| {
            self.ensure_writable("inode:finish_write")?;
            completed.finish(&mut self.device, &mut self.filesystem)
        });
        if !result
            .as_ref()
            .is_err_and(|error| error.requires_journal_progress())
        {
            self.writes.release(&receipt.identity);
            receipt.completed = None;
        }
        result
    }

    /// Releases a completed I/O owner when external journal progress failed.
    /// This does not claim that metadata was published or undo completed data.
    /// Only a data-completed receipt can enter this cancellation boundary.
    ///
    /// # Errors
    /// Rejects a foreign or already consumed receipt without changing its origin.
    pub fn discard_completed_inode_write(
        &mut self,
        receipt: &mut CompletedInodeWrite,
    ) -> Ext4Result<()> {
        if !self.device.owns_read_mount(&receipt.mount) {
            return Err(Ext4Error::invalid_input().with_operation("inode:foreign_write_receipt"));
        }
        self.writes.validate(&receipt.identity)?;
        self.writes.release(&receipt.identity);
        receipt.completed = None;
        Ok(())
    }
}
