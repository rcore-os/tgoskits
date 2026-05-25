//! Device filesystem implementation for ArceOS.
//!
//! The filesystem exposes a small static `/dev` tree and implements the
//! current `ax-fs-vfs` object model directly.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod device;
mod dir;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use ax_fs_vfs::{
    DirEntry, Filesystem, FilesystemOps, Reference, StatFs, VfsResult, path::MAX_NAME_LEN,
};
use spin::Once;

use self::dir::DevDirNode;

/// Creates a new mountable device filesystem handle.
pub fn new() -> Filesystem {
    DeviceFileSystem::new_filesystem()
}

/// Device filesystem.
pub struct DeviceFileSystem {
    next_inode: AtomicU64,
    root_dir: Once<DirEntry>,
}

impl DeviceFileSystem {
    /// Creates a new device filesystem object with `null`, `zero`, and `urandom`.
    pub fn new() -> Arc<Self> {
        let fs = Arc::new(Self {
            next_inode: AtomicU64::new(2),
            root_dir: Once::new(),
        });
        let root_fs = fs.clone();
        let root_dir = DirEntry::new_dir(
            |this| DevDirNode::make(root_fs.clone(), this, 1),
            Reference::root(),
        );
        fs.root_dir.call_once(|| root_dir.clone());
        root_dir
            .as_dir()
            .and_then(|dir| dir.downcast::<DevDirNode>())
            .expect("devfs root downcast failed")
            .populate_static_devices();
        fs
    }

    /// Creates a new mountable device filesystem handle.
    pub fn new_filesystem() -> Filesystem {
        Filesystem::new(Self::new())
    }

    pub(crate) fn alloc_inode(&self) -> u64 {
        self.next_inode.fetch_add(1, Ordering::Relaxed)
    }
}

impl FilesystemOps for DeviceFileSystem {
    fn name(&self) -> &str {
        "devfs"
    }

    fn root_dir(&self) -> DirEntry {
        self.root_dir.get().unwrap().clone()
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Ok(StatFs {
            fs_type: 0x1373,
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
