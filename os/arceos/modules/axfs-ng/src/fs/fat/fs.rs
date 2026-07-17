use alloc::sync::Arc;
use core::marker::PhantomPinned;

use axfs_ng_vfs::{
    DirEntry, Filesystem, FilesystemOps, Reference, StatFs, VfsResult, path::MAX_NAME_LEN,
};
use slab::Slab;

use super::{dir::FatDirNode, disk::SeekableDisk, ff, util::into_vfs_err};
use crate::{
    block::{BlockDevice, BlockRegion},
    os::sync::{PiMutex, PiMutexGuard, SpinMutex},
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
    inner: PiMutex<FatFilesystemInner>,
    root_dir: SpinMutex<Option<DirEntry>>,
}

impl FatFilesystem {
    pub fn new(dev: Arc<dyn BlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
        let disk = SeekableDisk::new(dev, region)?;
        let mut inner = FatFilesystemInner {
            inner: ff::FileSystem::new(disk, fatfs::FsOptions::new())
                .expect("failed to initialize FAT filesystem"),
            inode_allocator: Slab::new(),
            _pinned: PhantomPinned,
        };
        let root_inode = inner.alloc_inode();
        let result = Arc::new(Self {
            inner: PiMutex::new(inner),
            root_dir: SpinMutex::new(None),
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
        *result.root_dir.lock() = Some(root_dir);
        Ok(Filesystem::new(result))
    }
}

impl FatFilesystem {
    /// Locks the shared FAT state.
    ///
    /// FAT operations may perform block I/O while this guard is held, so
    /// contention must park the current task without disabling IRQs. Runtime
    /// filesystem initialization happens after the bootstrap task is installed.
    pub(crate) fn lock(&self) -> PiMutexGuard<'_, FatFilesystemInner> {
        self.inner.lock()
    }
}

impl FilesystemOps for FatFilesystem {
    fn name(&self) -> &str {
        "vfat"
    }

    fn root_dir(&self) -> DirEntry {
        self.root_dir.lock().clone().unwrap()
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
}
