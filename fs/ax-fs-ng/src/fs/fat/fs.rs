use alloc::{boxed::Box, sync::Arc};
use core::marker::PhantomPinned;

use axfs_ng_vfs::{
    DirEntry, Filesystem, FilesystemOps, Reference, StatFs, VfsResult, path::MAX_NAME_LEN,
};
use slab::Slab;

use super::{
    dir::FatDirNode,
    disk::{SeekableDisk, SeekableDiskFlusher},
    ff,
    util::into_vfs_err,
};
use crate::{
    block::{BlockRegion, FsBlockDevice},
    os::sync::{Mutex, MutexGuard, RawSpinLock},
};

pub struct FatFilesystemInner {
    pub inner: ff::FileSystem,
    inode_allocator: Slab<()>,
    _pinned: PhantomPinned,
}

impl FatFilesystemInner {
    pub(crate) fn alloc_inode(&mut self) -> u64 {
        self.inode_allocator.insert(()) as u64 + 1
    }

    pub(crate) fn release_inode(&mut self, ino: u64) {
        self.inode_allocator.remove(ino as usize - 1);
    }
}

pub struct FatFilesystem {
    inner: Mutex<FatFilesystemInner>,
    disk_flusher: SeekableDiskFlusher,
    root_dir: RawSpinLock<Option<DirEntry>>,
    /// Original device authority, retained while current writes are revoked.
    permissions: Option<ax_block::VolumePermissions>,
}

impl FatFilesystem {
    pub fn new(
        dev: Box<dyn FsBlockDevice>,
        region: BlockRegion,
        permissions: Option<ax_block::VolumePermissions>,
    ) -> VfsResult<Filesystem> {
        let disk = SeekableDisk::new(dev, region);
        let disk_flusher = disk.flusher();
        let mut inner = FatFilesystemInner {
            inner: ff::FileSystem::new(disk, fatfs::FsOptions::new())
                .expect("failed to initialize FAT filesystem"),
            inode_allocator: Slab::new(),
            _pinned: PhantomPinned,
        };
        let root_inode = inner.alloc_inode();
        let result = Arc::new(Self {
            inner: Mutex::new(inner),
            disk_flusher,
            root_dir: RawSpinLock::new(None),
            permissions,
        });

        let root_dir = DirEntry::new_dir(
            |this| {
                FatDirNode::new(
                    result.clone(),
                    result.lock().inner.root_dir(),
                    root_inode,
                    this,
                )
            },
            Reference::root(),
        );
        *result.root_dir.lock_irqsave() = Some(root_dir);
        Ok(Filesystem::new(result))
    }
}

impl FatFilesystem {
    /// Locks the shared FAT state.
    ///
    /// FAT operations may block on channel-backed block completion while this
    /// guard is held, so this state must never use an IRQ-disabling lock.
    pub(crate) fn lock(&self) -> MutexGuard<'_, FatFilesystemInner> {
        self.inner.lock()
    }
}

impl FilesystemOps for FatFilesystem {
    fn name(&self) -> &str {
        "vfat"
    }

    fn is_readonly(&self) -> bool {
        self.disk_flusher.is_read_only()
    }

    fn remount_read_write(&self) -> VfsResult<()> {
        // Every FAT operation holds this mutex. Keep permission restoration
        // serialized with disk I/O; publish VFS write access only on success.
        let _state = self.inner.lock();
        if !self.disk_flusher.is_read_only() {
            return Ok(());
        }
        let permissions = self
            .permissions
            .as_ref()
            .ok_or(axfs_ng_vfs::VfsError::PermissionDenied)?;
        permissions
            .restore_read_write()
            .map_err(|error| match error {
                crate::BlockError::Unsupported => axfs_ng_vfs::VfsError::PermissionDenied,
                error => crate::block_error_to_vfs_error(error),
            })?;
        if let Err(error) = self.disk_flusher.flush() {
            // Failed reconfiguration must retain the old read-only backend,
            // matching the caller's unchanged VFS state.
            permissions
                .restrict_read_only()
                .map_err(crate::block_error_to_vfs_error)?;
            return Err(crate::block_error_to_vfs_error(error));
        }
        Ok(())
    }

    fn root_dir(&self) -> DirEntry {
        self.root_dir.lock_irqsave().clone().unwrap()
    }

    fn stat(&self) -> VfsResult<StatFs> {
        let fs = self.inner.lock();
        let stats = fs.inner.stats().map_err(into_vfs_err)?;
        Ok(StatFs {
            fs_type: 0x65735546, // fuse
            block_size: stats.cluster_size() as _,
            blocks: stats.total_clusters() as _,
            blocks_free: stats.free_clusters() as _,
            blocks_available: stats.free_clusters() as _,

            file_count: 0,
            free_file_count: 0,

            name_length: MAX_NAME_LEN as _,
            fragment_size: 0,
            mount_flags: 0,
        })
    }

    fn flush(&self) -> VfsResult<()> {
        let _state = self.inner.lock();
        self.disk_flusher
            .flush()
            .map_err(crate::block_error_to_vfs_error)
    }

    fn shutdown(&self) -> VfsResult<()> {
        self.flush()
    }
}
