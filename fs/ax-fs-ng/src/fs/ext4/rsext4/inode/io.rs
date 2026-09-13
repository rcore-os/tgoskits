//! Inode-number-based data I/O, restartable resize, and extent operations.

use axfs_ng_vfs::{
    FileExtentMap as VfsFileExtentMap, FileExtentTarget as VfsFileExtentTarget, FileNodeOps,
    FileRangeOperation as VfsRangeOperation, PreallocationMode as VfsPreallocationMode, VfsResult,
};
use axpoll::{IoEvents, Pollable};
use rsext4::{PreallocationOptions, RangeOperation, ZeroRangeOptions};

use super::{Inode, into_vfs_err};

impl FileNodeOps for Inode {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let _inode = self.content_access().read()?;
        self.fs().read_inode(self.number(), offset, buf)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let _inode = self.content_access().write()?;
        // Use inode-number-based write so open-unlinked regular files remain
        // writable after their directory entry has been removed.
        self.write_locked(buf, offset).map_err(into_vfs_err)?;
        self.finish_metadata_change()?;
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let _inode = self.content_access().write()?;
        // The chosen append offset is stable across log-space waits. Every
        // independently opened handle for this inode shares content-write exclusion.
        let length = self
            .fs()
            .lock()
            .ext4
            .inode(self.number())
            .map_err(into_vfs_err)?
            .size;
        self.write_locked(buf, length).map_err(into_vfs_err)?;
        self.finish_metadata_change()?;
        let end = length
            .checked_add(buf.len() as u64)
            .ok_or_else(|| into_vfs_err(rsext4::Ext4Error::overflow()))?;
        Ok((buf.len(), end))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        let _inode = self.content_access().write()?;
        // An open-unlinked regular file stays alive by inode number, not by a
        // directory entry.
        let mut resize = rsext4::InodeResize::new(self.number(), len);
        self.mutate(|ext4| ext4.resize_inode(&mut resize))
            .map_err(into_vfs_err)?;
        self.finish_metadata_change()
    }

    fn operate_range(&self, offset: u64, len: u64, operation: VfsRangeOperation) -> VfsResult<()> {
        let operation = match operation {
            VfsRangeOperation::Allocate(mode) => RangeOperation::Allocate(match mode {
                VfsPreallocationMode::ExtendSize => PreallocationOptions::EXTEND_SIZE,
                VfsPreallocationMode::KeepSize => PreallocationOptions::KEEP_SIZE,
            }),
            VfsRangeOperation::PunchHole => RangeOperation::PunchHole,
            VfsRangeOperation::ZeroRange(mode) => RangeOperation::Zero(match mode {
                VfsPreallocationMode::ExtendSize => ZeroRangeOptions::EXTEND_SIZE,
                VfsPreallocationMode::KeepSize => ZeroRangeOptions::KEEP_SIZE,
            }),
            VfsRangeOperation::CollapseRange => RangeOperation::Collapse,
            VfsRangeOperation::InsertRange => RangeOperation::Insert,
        };
        let _inode = self.content_access().write()?;
        self.mutate(|ext4| ext4.operate_inode_range(self.number(), offset, len, operation))
            .map_err(into_vfs_err)?;
        self.finish_metadata_change()
    }

    fn map_extents(
        &self,
        offset: u64,
        len: u64,
        target: VfsFileExtentTarget,
        extent_limit: usize,
    ) -> VfsResult<VfsFileExtentMap> {
        self.inspect_extents(offset, len, target, extent_limit)
    }
}

impl Pollable for Inode {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}

impl Inode {
    fn write_locked(&self, bytes: &[u8], offset: u64) -> rsext4::Ext4Result<()> {
        if self
            .fs()
            .lock()
            .ext4
            .inode_has_restartable_writes(self.number())?
        {
            self.fs().write_extent_inode(self.number(), offset, bytes)
        } else {
            self.fs().write_legacy_inode(self.number(), offset, bytes)
        }
    }
}
