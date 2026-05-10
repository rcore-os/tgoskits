use alloc::sync::Arc;
use core::cell::OnceCell;

use ax_driver::{AxBlockDevice, PartitionRegion};
use ax_kspin::{SpinNoPreempt as Mutex, SpinNoPreemptGuard as MutexGuard};
use axfs_ng_vfs::{
    DirEntry, DirNode, Filesystem, FilesystemOps, Reference, StatFs, VfsResult, path::MAX_NAME_LEN,
};
use rsext4::{Jbd2Dev, bmalloc::InodeNumber};

use super::{Ext4Disk, Inode, util::into_vfs_err};

const EXT4_ROOT_INO: u32 = 2;

pub(crate) struct Ext4State {
    pub fs: rsext4::Ext4FileSystem,
    pub dev: Jbd2Dev<Ext4Disk>,
}

impl Ext4State {
    pub(crate) fn split(&mut self) -> (&mut rsext4::Ext4FileSystem, &mut Jbd2Dev<Ext4Disk>) {
        let fs = &mut self.fs as *mut _;
        let dev = &mut self.dev as *mut _;
        unsafe { (&mut *fs, &mut *dev) }
    }
}

pub struct Ext4Filesystem {
    inner: Mutex<Ext4State>,
    root_dir: OnceCell<DirEntry>,
}

impl Ext4Filesystem {
    pub fn new(dev: AxBlockDevice, region: PartitionRegion) -> VfsResult<Filesystem> {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, Ext4Disk::new(dev, region), true);
        let fs = rsext4::mount(&mut dev).map_err(into_vfs_err)?;

        let fs = Arc::new(Self {
            inner: Mutex::new(Ext4State { fs, dev }),
            root_dir: OnceCell::new(),
        });
        let _ = fs.root_dir.set(DirEntry::new_dir(
            |this| {
                DirNode::new(Inode::new(
                    fs.clone(),
                    InodeNumber::new(EXT4_ROOT_INO).unwrap(),
                    Some(this),
                    Some("/".into()),
                ))
            },
            Reference::root(),
        ));
        Ok(Filesystem::new(fs))
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Ext4State> {
        self.inner.lock()
    }

    pub(crate) fn sync_to_disk(&self) -> VfsResult<()> {
        let mut state = self.inner.lock();
        let (fs, dev) = state.split();
        fs.datablock_cache.flush_all(dev).map_err(into_vfs_err)?;
        fs.bitmap_cache.flush_all(dev).map_err(into_vfs_err)?;
        fs.inodetable_cahce.flush_all(dev).map_err(into_vfs_err)?;
        fs.sync_superblock(dev).map_err(into_vfs_err)?;
        fs.sync_group_descriptors(dev).map_err(into_vfs_err)?;
        if dev.is_use_journal() {
            dev.umount_commit();
        }
        dev.cantflush().map_err(into_vfs_err)
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
        let state = self.lock();
        let superblock = &state.fs.superblock;
        let block_size = superblock.block_size();
        let blocks = superblock.blocks_count();
        let blocks_free = superblock.free_blocks_count();
        Ok(StatFs {
            fs_type: 0xef53,
            block_size: block_size as _,
            blocks,
            blocks_free,
            blocks_available: blocks_free,
            file_count: superblock.s_inodes_count as _,
            free_file_count: superblock.s_free_inodes_count as _,
            name_length: MAX_NAME_LEN as _,
            fragment_size: 0,
            mount_flags: 0,
        })
    }

    fn flush(&self) -> VfsResult<()> {
        self.sync_to_disk()
    }
}
