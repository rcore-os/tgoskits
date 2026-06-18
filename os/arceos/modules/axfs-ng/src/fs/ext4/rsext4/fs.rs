use alloc::{boxed::Box, sync::Arc};
use core::cell::OnceCell;

use axfs_ng_vfs::{
    DirEntry, DirNode, Filesystem, FilesystemOps, Reference, StatFs, VfsResult, path::MAX_NAME_LEN,
};
use rsext4::{
    Jbd2Dev, MountOptions, bmalloc::InodeNumber, error::Errno, superblock::Ext4Superblock,
};

use super::{Ext4Disk, Inode, util::into_vfs_err};
use crate::{
    block::{BlockRegion, FsBlockDevice},
    os::sync::{SleepMutex as Mutex, SleepMutexGuard as MutexGuard},
};

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
    readonly: bool,
}

impl Ext4Filesystem {
    pub fn new(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> VfsResult<Filesystem> {
        Self::new_from_boxed(dev, region)
    }

    /// Create from a dynamic (boxed) block device (e.g. loop device).
    pub fn new_from_boxed(
        dev: Box<dyn FsBlockDevice>,
        region: BlockRegion,
    ) -> VfsResult<Filesystem> {
        let disk = Ext4Disk::new(dev, region);
        let mut dev = Jbd2Dev::initial_jbd2dev(0, disk, true);
        let (fs, dev, readonly) = match rsext4::Ext4FileSystem::device_has_error_state(&mut dev) {
            Ok(true) => {
                warn!(
                    "ext4 filesystem is in error state; mounting read-only without journal replay"
                );
                Self::mount_readonly_no_replay(dev)?
            }
            Ok(false) => match rsext4::mount(&mut dev) {
                Ok(fs) => (fs, dev, false),
                Err(err) if err.code == Errno::EUCLEAN => {
                    warn!(
                        "ext4 journal replay failed with EUCLEAN; retrying read-only without \
                         journal replay"
                    );
                    Self::mount_readonly_no_replay(dev)?
                }
                Err(err) => return Err(into_vfs_err(err)),
            },
            Err(err) if err.code == Errno::EUCLEAN => {
                warn!(
                    "ext4 superblock check failed with EUCLEAN; retrying read-only without \
                     journal replay"
                );
                Self::mount_readonly_no_replay(dev)?
            }
            Err(err) => return Err(into_vfs_err(err)),
        };

        let fs = Arc::new(Self {
            inner: Mutex::new(Ext4State { fs, dev }),
            root_dir: OnceCell::new(),
            readonly,
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

    fn mount_readonly_no_replay(
        dev: Jbd2Dev<Ext4Disk>,
    ) -> VfsResult<(rsext4::Ext4FileSystem, Jbd2Dev<Ext4Disk>, bool)> {
        let mut dev = Jbd2Dev::initial_jbd2dev(0, dev.into_inner(), false);
        let fs = rsext4::mount_with_options(&mut dev, MountOptions::read_only_no_journal_replay())
            .map_err(into_vfs_err)?;
        Ok((fs, dev, true))
    }

    /// Locks the shared rsext4 state.
    ///
    /// Uses a blocking mutex because rsext4 operations may issue block I/O while
    /// this guard is held. Submit/poll block devices without IRQ support can
    /// yield while waiting for completion, so the outer filesystem state guard
    /// must not disable interrupts or preemption.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Ext4State> {
        self.inner.lock()
    }

    pub(crate) fn sync_to_disk(&self) -> VfsResult<()> {
        if self.readonly {
            return Ok(());
        }

        let mut state = self.inner.lock();
        let (fs, dev) = state.split();
        fs.datablock_cache.flush_all(dev).map_err(into_vfs_err)?;
        fs.bitmap_cache.flush_all(dev).map_err(into_vfs_err)?;
        fs.inodetable_cache.flush_all(dev).map_err(into_vfs_err)?;
        // Mark the filesystem clean before writing the superblock so the
        // on-disk state reflects a clean sync / unmount.
        fs.superblock.s_state = Ext4Superblock::EXT4_VALID_FS;
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

    fn is_readonly(&self) -> bool {
        self.readonly
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
