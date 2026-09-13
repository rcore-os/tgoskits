//! Inode lifetime, per-inode I/O exclusion, and shared ext4 mutation boundaries.

use alloc::sync::Arc;

use axfs_ng_vfs::{
    FileExtent as VfsFileExtent, FileExtentMap as VfsFileExtentMap,
    FileExtentState as VfsFileExtentState, FileExtentTarget as VfsFileExtentTarget, NodeOps,
    VfsResult, WeakDirEntry, WritebackPolicy,
};
use rsext4::InodeNumber;

use super::{
    Ext4Filesystem,
    fs::{AccessGate, InodeLifetime, LocatedInode},
    util::into_vfs_err,
};

mod directory;
mod io;
mod metadata;
mod xattr;

pub struct Inode {
    lifetime: InodeLifetime,
    this: Option<WeakDirEntry>,
}

impl Inode {
    pub(crate) fn new(lifetime: InodeLifetime, this: Option<WeakDirEntry>) -> Arc<Self> {
        Arc::new(Self { lifetime, this })
    }

    fn fs(&self) -> &Arc<Ext4Filesystem> {
        self.lifetime.filesystem()
    }

    fn number(&self) -> InodeNumber {
        self.lifetime.number()
    }

    fn content_access(&self) -> &Arc<AccessGate> {
        self.lifetime.content_access()
    }

    fn mutate<T>(
        &self,
        mut operation: impl FnMut(&mut super::MountedExt4) -> rsext4::Ext4Result<T>,
    ) -> rsext4::Ext4Result<T> {
        self.fs()
            .with_writeback_progress(|state| operation(&mut state.ext4))
    }

    fn finish_metadata_change(&self) -> VfsResult<()> {
        if !self.fs().background_writeback_enabled()
            || self
                .writeback_policy()?
                .contains(WritebackPolicy::SYNCHRONOUS)
        {
            self.fs().sync_to_disk()?;
        }
        Ok(())
    }

    fn finish_directory_change(&self) -> VfsResult<()> {
        if !self.fs().background_writeback_enabled() || self.writeback_policy()?.syncs_directory() {
            self.fs().sync_to_disk()?;
        }
        Ok(())
    }

    fn inspect_extents(
        &self,
        offset: u64,
        len: u64,
        target: VfsFileExtentTarget,
        extent_limit: usize,
    ) -> VfsResult<VfsFileExtentMap> {
        let mut state = self.fs().lock();
        let mappings = state
            .ext4
            .inode_extents(
                self.number(),
                offset,
                len,
                match target {
                    VfsFileExtentTarget::Data => rsext4::FileExtentTarget::Data,
                    VfsFileExtentTarget::ExtendedAttributes => {
                        rsext4::FileExtentTarget::ExtendedAttributes
                    }
                },
                extent_limit,
            )
            .map_err(into_vfs_err)?;
        Ok(VfsFileExtentMap {
            mapped_extents: mappings.mapped_extents,
            complete: mappings.complete,
            extents: mappings
                .extents
                .into_iter()
                .map(|extent| VfsFileExtent {
                    logical_start: extent.logical_start,
                    physical_start: extent.physical_start,
                    length: extent.length,
                    state: match extent.state {
                        rsext4::FileExtentState::Initialized => VfsFileExtentState::Initialized,
                        rsext4::FileExtentState::Unwritten => VfsFileExtentState::Unwritten,
                        rsext4::FileExtentState::Inline => VfsFileExtentState::Inline,
                    },
                    merged: extent.merged,
                })
                .collect(),
        })
    }
}
