//! Owned, OS-independent mounted filesystem boundary.

mod block_read;
mod directory;
mod directory_read;
mod inode;
mod inode_load;
mod inode_writeback;
mod metadata;
mod namespace;
mod read;
mod shutdown;
mod write;
mod writeback;
use alloc::{collections::VecDeque, vec::Vec};

use bitflags::bitflags;
pub use block_read::{InodeBlockRequest, InodeDataRequest, InodeReadCache};
pub use directory::*;
pub use directory_read::*;
pub use inode::*;
pub use inode_load::{CompletedLiveInodeRead, LiveInodeRead, PreparedLiveInodeRead};
pub use inode_writeback::{CompletedInodeTableRead, PreparedInodeTableRead};
pub use metadata::InodeMetadataReader;
pub use read::{CompletedInodeRead, InodeReadPreparation, PreparedInodeRead, ValidatedInodeRead};
pub use shutdown::{PreparedUnmount, UnmountReceipt};
pub use write::{CompletedInodeWrite, PreparedInodeWrite};

use super::{Ext4FileSystem, FileSystemStats, MkfsOptions, MountOptions, mkfs_with_options};
use crate::{
    blockdev::Jbd2Dev,
    bmalloc::InodeNumber,
    checksum::{verify_ext4_dirblock_checksum, verify_ext4_dx_checksum},
    dir::{CreateEntryRequest, FileName, LinkEntryRequest, create_directory_at},
    disknode::{DeviceNumber, Ext4Inode, Ext4TimeSpec, Ext4Timestamp},
    entries::Ext4DirEntry2,
    error::{Ext4Error, Ext4ErrorKind, Ext4Result},
    file::{
        CreateInodePayload, FileExtentMap, FileExtentTarget, PreallocationOptions, RangeOperation,
        RenameEntryRequest, RenameOptions, RenameOutcome, UnlinkOutcome, XattrName, XattrNamespace,
        XattrSetMode, create_inode_at, error_after_cleanup, find_named_entry_in_parent,
        get_inode_xattr, inspect_inode_extents, link_inode_at, list_inode_xattrs,
        operate_inode_range, read_inode_data_into, reap_unlinked_inode, remove_inode_xattr,
        rename_inode_at, set_inode_xattr, truncate_inode, unlink_empty_directory_at,
        unlink_inode_at, write_inode_data,
    },
    hashtree::{
        Ext4InodeHashTreeExt, IndexedDirectoryRange, IndexedDirectoryRecord,
        read_indexed_directory_range,
    },
    io::BlockIo,
    loopfile::resolve_inode_blocks,
    metadata::{Ext4InodeMetadataUpdate, Ext4MetadataReason, Ext4ModeUpdate},
    runtime::{Clock, MountServices, MountedServices, Observer},
};

/// Mounted ext4 instance that owns its device, caches, journal, and services.
///
/// The representation is private. The embedding OS serializes access to this
/// value with a sleepable lock when sharing a mount between tasks.
pub struct Ext4<D: BlockIo, S> {
    filesystem: Ext4FileSystem,
    device: Jbd2Dev<D>,
    services: S,
    options: MountOptions,
    shutdown_ticket: Option<crate::SyncTicket>,
    unmount_error: Option<Ext4Error>,
    writes: write::InodeWriteOwners,
}

/// Formats a device with an OS-independent clock and returns device ownership.
pub fn format<D, C>(device: D, clock: C, options: MkfsOptions) -> Ext4Result<D>
where
    D: BlockIo,
    C: Clock + Send + 'static,
{
    let mut device = Jbd2Dev::with_clock(0, device, clock, true);
    mkfs_with_options(&mut device, options)?;
    Ok(device.into_inner())
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Mounts an ext4 filesystem and transfers ownership of all capabilities.
    pub fn mount<C>(
        device: D,
        services: MountServices<C, E, O, W>,
        options: MountOptions,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
    {
        Self::mount_selecting_options(device, services, |_| Ok(options))
    }

    /// Selects read-only replay before mounting when the on-disk filesystem
    /// has recorded errors; otherwise performs one read-write mount attempt.
    ///
    /// A failed mount is never retried through the same journal/cache owner.
    /// Replay may already have updated home blocks or latched an abort, so a
    /// second attempt would lose the first error and observe polluted state.
    pub fn mount_with_readonly_fallback<C>(
        device: D,
        services: MountServices<C, E, O, W>,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
    {
        Self::mount_selecting_options(device, services, |device| {
            let read_write = MountOptions::read_write();
            let read_only_replay = MountOptions {
                readonly: true,
                replay_journal: true,
                block_validity: true,
            };
            if Ext4FileSystem::device_has_error_state(device)? {
                Ok(read_only_replay)
            } else {
                Ok(read_write)
            }
        })
    }

    fn mount_selecting_options<C, F>(
        device: D,
        services: MountServices<C, E, O, W>,
        select_options: F,
    ) -> Ext4Result<Self>
    where
        C: Clock + Send + 'static,
        F: FnOnce(&mut Jbd2Dev<D>) -> Ext4Result<MountOptions>,
    {
        let MountServices {
            clock,
            mut entropy,
            mut observer,
            mut mmp_delay,
            mmp_identity,
        } = services;
        let mut device = Jbd2Dev::with_clock(0, device, clock, true);
        let options = select_options(&mut device)?;
        let filesystem = Ext4FileSystem::mount_with_services(
            &mut device,
            options,
            &mut observer,
            &mut entropy,
            &mut mmp_delay,
            mmp_identity,
        )?;

        Ok(Self {
            filesystem,
            device,
            services: MountedServices::new(entropy, observer, mmp_delay, mmp_identity),
            options,
            shutdown_ticket: None,
            unmount_error: None,
            writes: write::InodeWriteOwners::default(),
        })
    }
}

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    pub const fn options(&self) -> MountOptions {
        self.options
    }

    /// Applies mount options without releasing device or journal ownership.
    pub fn remount(&mut self, options: MountOptions) -> Ext4Result<()> {
        self.writes.ensure_drained()?;
        if !self.filesystem.mounted {
            return Err(Ext4Error::busy().with_operation("remount:unmounted"));
        }
        if options.replay_journal != self.options.replay_journal {
            return Err(Ext4Error::unsupported().with_operation("remount:replay_policy"));
        }

        let previous_options = self.options;
        self.filesystem
            .set_block_validity(&mut self.device, options.block_validity)?;
        let mode_result = match (previous_options.readonly, options.readonly) {
            (false, true) => self.remount_read_only(),
            (true, false) => self.remount_read_write(),
            _ => Ok(()),
        };
        if let Err(error) = mode_result {
            let rollback = self
                .filesystem
                .set_block_validity(&mut self.device, previous_options.block_validity);
            return Err(error_after_cleanup(error, rollback));
        }
        self.options = options;
        Ok(())
    }

    pub fn root_inode(&self) -> InodeNumber {
        self.filesystem.root_inode
    }

    pub fn statfs(&self) -> FileSystemStats {
        self.filesystem.statfs()
    }

    pub fn sync(&mut self) -> Ext4Result<()> {
        self.ensure_mounted("sync:unmounted")?;
        if self.options.readonly {
            return self.device.flush();
        }
        self.filesystem.mmp.ensure_writable("sync:mmp_failed")?;
        self.filesystem
            .sync_filesystem_with_observer(&mut self.device, &mut self.services.observer)
    }

    /// Refreshes MMP ownership after the embedding runtime's lock-free wait.
    ///
    /// `elapsed` is the monotonic duration since the previous successful MMP
    /// publication. The caller must not hold this mount's outer lock while
    /// waiting for the returned interval.
    pub fn refresh_mmp(
        &mut self,
        elapsed: core::time::Duration,
    ) -> Ext4Result<Option<core::time::Duration>> {
        if self.options.readonly || !self.filesystem.mmp.is_active() {
            return Ok(None);
        }
        let interval = self.filesystem.mmp.refresh(
            &mut self.device,
            &self.filesystem.superblock,
            self.services.mmp_identity,
            elapsed,
        )?;
        Ok(Some(interval))
    }

    /// Returns the periodic MMP interval without performing I/O.
    pub const fn mmp_refresh_interval(&self) -> Option<core::time::Duration> {
        self.filesystem.mmp.refresh_interval()
    }

    /// Latches loss of the embedding runtime's periodic MMP driver.
    ///
    /// Once reported, all subsequent mutations fail until a new mount owns a
    /// functioning runtime driver.
    pub fn report_mmp_runtime_failure(&mut self, error: Ext4Error) {
        if self.filesystem.mmp.is_active() {
            self.filesystem.mmp.mark_failed(error);
        }
    }

    /// Persists a clean filesystem and then releases writable MMP ownership.
    ///
    /// If the final MMP write fails, the ext4/JBD2 state is already clean and
    /// this mount becomes terminal: further mutations and remounts are
    /// rejected. Retrying an uncertain CLEAN write could overwrite a new MMP
    /// owner that claimed the device after observing the first write.
    pub fn unmount(&mut self) -> Ext4Result<()> {
        self.writes.ensure_drained()?;
        self.ensure_mounted("unmount:unmounted")?;
        if self.options.readonly {
            self.filesystem
                .finish_read_only_unmount(&mut self.services.observer);
            return Ok(());
        }
        let result = self
            .filesystem
            .umount_with_observer(&mut self.device, &mut self.services.observer)
            .and_then(|()| self.release_mmp());
        if !self.filesystem.mounted {
            self.unmount_error = result.err();
        }
        result
    }

    fn ensure_writable(&self, operation: &'static str) -> Ext4Result<()> {
        self.ensure_mounted(operation)?;
        if self.options.readonly {
            Err(Ext4Error::read_only().with_operation(operation))
        } else {
            self.filesystem.mmp.ensure_writable(operation)?;
            self.device.ensure_mutation_admitted()
        }
    }

    fn ensure_mounted(&self, operation: &'static str) -> Ext4Result<()> {
        if let Some(error) = self.unmount_error {
            return Err(error);
        }
        if self.filesystem.mounted {
            Ok(())
        } else {
            // A failed final MMP release is a terminal unmounted state, but
            // the loss of ownership is only the consequence.  Keep reporting
            // the latched I/O failure instead of hiding it behind EBUSY.
            self.filesystem.mmp.ensure_writable(operation)?;
            Err(Ext4Error::busy().with_operation(operation))
        }
    }

    fn remount_read_only(&mut self) -> Ext4Result<()> {
        self.filesystem.remount_read_only(&mut self.device)?;
        match self.release_mmp() {
            Ok(()) => Ok(()),
            Err(error) => self.rollback_read_only_transition(error),
        }
    }

    fn remount_read_write(&mut self) -> Ext4Result<()> {
        self.claim_mmp()?;
        if let Err(error) = self
            .filesystem
            .remount_read_write(&mut self.device, &mut self.services.observer)
        {
            return Err(error_after_cleanup(error, self.release_mmp()));
        }
        if let Err(error) = self.refresh_claimed_mmp() {
            let remount_cleanup = self.filesystem.remount_read_only(&mut self.device);
            let release_cleanup = self.release_mmp();
            let cleanup_error = error_after_cleanup(error, remount_cleanup);
            return Err(error_after_cleanup(cleanup_error, release_cleanup));
        }
        Ok(())
    }

    fn claim_and_refresh_mmp(&mut self) -> Ext4Result<()> {
        self.claim_mmp()?;
        match self.refresh_claimed_mmp() {
            Ok(()) => Ok(()),
            Err(error) => Err(error_after_cleanup(error, self.release_mmp())),
        }
    }

    fn claim_mmp(&mut self) -> Ext4Result<()> {
        self.filesystem.mmp = super::mmp::MmpState::claim(
            &mut self.device,
            &self.filesystem.superblock,
            &mut self.services.entropy,
            &mut self.services.mmp_delay,
        )?;
        Ok(())
    }

    fn refresh_claimed_mmp(&mut self) -> Ext4Result<()> {
        if !self.filesystem.mmp.is_active() {
            return Ok(());
        }
        self.filesystem.mmp.refresh(
            &mut self.device,
            &self.filesystem.superblock,
            self.services.mmp_identity,
            core::time::Duration::ZERO,
        )?;
        Ok(())
    }

    fn release_mmp(&mut self) -> Ext4Result<()> {
        self.filesystem
            .mmp
            .release_clean(&mut self.device, &self.filesystem.superblock)
    }

    fn rollback_read_only_transition(&mut self, operation_error: Ext4Error) -> Ext4Result<()> {
        if let Err(reclaim_error) = self.claim_and_refresh_mmp() {
            self.filesystem.mmp.mark_failed(reclaim_error);
            return Err(reclaim_error);
        }
        if let Err(remount_error) = self
            .filesystem
            .remount_read_write(&mut self.device, &mut self.services.observer)
        {
            self.filesystem.mmp.mark_failed(remount_error);
            return Err(remount_error);
        }
        Err(operation_error)
    }
}

#[cfg(test)]
mod tests;
