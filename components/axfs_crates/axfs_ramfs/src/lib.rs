//! In-memory filesystem implementation for ArceOS.
//!
//! This crate implements the current `ax-fs-vfs` object model directly.  It is
//! intentionally small: directory entries live in memory and regular file
//! contents are stored in growable byte buffers.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod dir;
mod file;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use ax_fs_vfs::{
    DirEntry, Filesystem, FilesystemOps, Reference, StatFs, VfsResult, path::MAX_NAME_LEN,
};
use spin::Once;

use self::dir::RamDirNode;

/// Creates a new mountable RAM filesystem handle.
pub fn new() -> Filesystem {
    RamFileSystem::new_filesystem()
}

/// In-memory filesystem.
pub struct RamFileSystem {
    next_inode: AtomicU64,
    root_dir: Once<DirEntry>,
}

impl RamFileSystem {
    /// Creates a new RAM filesystem object.
    pub fn new() -> Arc<Self> {
        let fs = Arc::new(Self {
            next_inode: AtomicU64::new(2),
            root_dir: Once::new(),
        });
        let root_fs = fs.clone();
        fs.root_dir.call_once(|| {
            DirEntry::new_dir(
                |this| RamDirNode::make(root_fs, this, 1, ax_fs_vfs::NodePermission::default()),
                Reference::root(),
            )
        });
        fs
    }

    /// Creates a new mountable RAM filesystem handle.
    pub fn new_filesystem() -> Filesystem {
        Filesystem::new(Self::new())
    }

    pub(crate) fn alloc_inode(&self) -> u64 {
        self.next_inode.fetch_add(1, Ordering::Relaxed)
    }
}

impl FilesystemOps for RamFileSystem {
    fn name(&self) -> &str {
        "ramfs"
    }

    fn root_dir(&self) -> DirEntry {
        self.root_dir.get().unwrap().clone()
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Ok(StatFs {
            fs_type: 0x8584_58f6,
            block_size: 4096,
            blocks: 0,
            blocks_free: 0,
            blocks_available: 0,
            file_count: self.next_inode.load(Ordering::Relaxed),
            free_file_count: 0,
            name_length: MAX_NAME_LEN as u32,
            fragment_size: 4096,
            mount_flags: 0,
        })
    }
}
