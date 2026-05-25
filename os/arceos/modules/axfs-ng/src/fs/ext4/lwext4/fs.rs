use alloc::{boxed::Box, sync::Arc};
use core::cell::OnceCell;

use ax_kspin::{SpinNoIrq as Mutex, SpinNoIrqGuard as MutexGuard};
use axfs_ng_vfs::{
    DirEntry, DirNode, Filesystem, FilesystemOps, Reference, StatFs, VfsResult, path::MAX_NAME_LEN,
};
use lwext4_rust::{FsConfig, ffi::EXT4_ROOT_INO};

use super::{
    Ext4Disk, Inode,
    util::{LwExt4Filesystem, into_vfs_err},
};
use crate::block::{BlockRegion, FsBlockDevice};

const EXT4_CONFIG: FsConfig = FsConfig { bcache_size: 256 };

pub struct Ext4Filesystem {
    inner: Mutex<LwExt4Filesystem>,
    root_dir: OnceCell<DirEntry>,
}

impl Ext4Filesystem {
    pub fn new(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
        let ext4 = lwext4_rust::Ext4Filesystem::new(Ext4Disk::new(dev, region), EXT4_CONFIG)
            .map_err(into_vfs_err)?;

        let fs = Arc::new(Self {
            inner: Mutex::new(ext4),
            root_dir: OnceCell::new(),
        });
        let _ = fs.root_dir.set(DirEntry::new_dir(
            |this| DirNode::new(Inode::new(fs.clone(), EXT4_ROOT_INO, Some(this))),
            Reference::root(),
        ));
        Ok(Filesystem::new(fs))
    }

    /// Locks the shared lwext4 state.
    ///
    /// lwext4 operations may call into the block device while this guard is
    /// held. The current rootfs setup can also run in early atomic contexts
    /// where a blocking mutex trips `might_sleep()`, so use `SpinNoIrq`
    /// instead of the older `SpinNoPreempt` to close same-CPU IRQ reentry
    /// without changing the boot-time calling contract.
    pub(crate) fn lock(&self) -> MutexGuard<'_, LwExt4Filesystem> {
        self.inner.lock()
    }
}

unsafe impl Send for Ext4Filesystem {}

unsafe impl Sync for Ext4Filesystem {}

impl FilesystemOps for Ext4Filesystem {
    fn name(&self) -> &str {
        "ext4"
    }

    fn root_dir(&self) -> DirEntry {
        self.root_dir.get().unwrap().clone()
    }

    fn stat(&self) -> VfsResult<StatFs> {
        let mut fs = self.lock();
        let stat = fs.stat().map_err(into_vfs_err)?;
        Ok(StatFs {
            fs_type: 0xef53,
            block_size: stat.block_size as _,
            blocks: stat.blocks_count,
            blocks_free: stat.free_blocks_count,
            blocks_available: stat.free_blocks_count,

            file_count: stat.inodes_count as _,
            free_file_count: stat.free_inodes_count as _,

            name_length: MAX_NAME_LEN as _,
            fragment_size: 0,
            mount_flags: 0,
        })
    }

    fn flush(&self) -> VfsResult<()> {
        self.inner.lock().flush().map_err(into_vfs_err)
    }
}
