use alloc::{
    boxed::Box,
    collections::BTreeMap,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    marker::PhantomPinned,
    ops::{Deref, DerefMut},
};

use axfs_ng_vfs::{
    DirEntry, Filesystem, FilesystemOps, Reference, StatFs, VfsResult, WeakDirEntry,
    path::MAX_NAME_LEN,
};
use slab::Slab;

use super::{
    dir::FatDirNode,
    disk::{SeekableDisk, SeekableDiskFlusher},
    ff,
    state::FatInode,
    util::into_vfs_err,
};
use crate::{
    block::{BlockRegion, FsBlockDevice},
    os::sync::{Mutex, MutexGuard, RawSpinLock},
};

pub struct FatFilesystemInner {
    pub inner: ff::FileSystem,
    inode_allocator: Slab<()>,
    pub(super) active: BTreeMap<(u64, String), Weak<FatInode>>,
    next_generation: u64,
    _pinned: PhantomPinned,
}

impl FatFilesystemInner {
    pub(super) fn alloc_identity(&mut self) -> VfsResult<(u64, u64)> {
        let generation = self.next_generation;
        self.next_generation = generation
            .checked_add(1)
            .ok_or(axfs_ng_vfs::VfsError::ValueOverflow)?;
        Ok((self.alloc_inode(), generation))
    }
    pub(crate) fn alloc_inode(&mut self) -> u64 {
        self.inode_allocator.insert(()) as u64 + 1
    }

    pub(crate) fn release_inode(&mut self, ino: u64) {
        self.inode_allocator.remove(ino as usize - 1);
    }
}

#[derive(Default)]
struct RootIndex {
    entry: Option<WeakDirEntry>,
    holder: Weak<FatInode>,
}

pub struct FatFilesystem {
    self_ref: Weak<Self>,
    inner: Mutex<FatFilesystemInner>,
    disk_flusher: SeekableDiskFlusher,
    root_dir: RawSpinLock<RootIndex>,
    pub(super) root_inode: u64,
    /// Original device authority, retained while current writes are revoked.
    permissions: Option<ax_block::VolumePermissions>,
}

/// Owner evidence for every access to lifetime-erased fatfs handles.
pub(super) struct FatFilesystemGuard<'a> {
    held: Option<MutexGuard<'a, FatFilesystemInner>>,
    owner: &'a FatFilesystem,
    retained: Vec<Arc<FatInode>>,
    retired: Option<&'a axfs_ng_vfs::MutationRetire>,
}

impl FatFilesystemGuard<'_> {
    pub(super) fn owner_id(&self) -> usize {
        core::ptr::from_ref(self.owner) as usize
    }

    pub(super) fn retain(&mut self, inode: &Arc<FatInode>) {
        self.retained.push(inode.clone());
    }
}

impl Deref for FatFilesystemGuard<'_> {
    type Target = FatFilesystemInner;
    fn deref(&self) -> &Self::Target {
        self.held.as_deref().expect("live FAT guard")
    }
}

impl DerefMut for FatFilesystemGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.held.as_deref_mut().expect("live FAT guard")
    }
}

impl Drop for FatFilesystemGuard<'_> {
    fn drop(&mut self) {
        // An inode destructor acquires this mutex. Never become its last owner
        // while still holding the filesystem guard.
        drop(self.held.take());
        if let Some(retired) = self.retired {
            retired.retire(core::mem::take(&mut self.retained));
        } else {
            self.retained.clear();
        }
    }
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
            active: BTreeMap::new(),
            next_generation: 1,
            _pinned: PhantomPinned,
        };
        let root_inode = inner.alloc_inode();
        let result = Arc::new_cyclic(|self_ref| Self {
            self_ref: self_ref.clone(),
            inner: Mutex::new(inner),
            disk_flusher,
            root_dir: RawSpinLock::new(RootIndex::default()),
            root_inode,
            permissions,
        });
        Ok(Filesystem::new(result))
    }
}

impl FatFilesystem {
    fn root_holder(&self) -> Arc<FatInode> {
        // Never hold the root index while acquiring the sleepable filesystem
        // mutex. That mutex canonicalizes raw holders; the short index lock
        // only publishes weak references.
        let mut guard = self.lock();
        let existing = self.root_dir.lock_irqsave().holder.upgrade();
        let holder = existing.unwrap_or_else(|| {
            FatInode::root(
                self.self_ref.upgrade().expect("live FAT filesystem owner"),
                guard.inner.root_dir(),
                self.root_inode,
            )
        });
        guard.retain(&holder);
        let retired = {
            let mut index = self.root_dir.lock_irqsave();
            core::mem::replace(&mut index.holder, Arc::downgrade(&holder))
        };
        drop(guard);
        drop(retired);
        holder
    }

    /// Locks the shared FAT state.
    ///
    /// FAT operations may block on channel-backed block completion while this
    /// guard is held, so this state must never use an IRQ-disabling lock.
    pub(super) fn lock(&self) -> FatFilesystemGuard<'_> {
        FatFilesystemGuard {
            held: Some(self.inner.lock()),
            owner: self,
            retained: Vec::new(),
            retired: None,
        }
    }

    pub(super) fn lock_retiring<'a>(
        &'a self,
        retired: &'a axfs_ng_vfs::MutationRetire,
    ) -> FatFilesystemGuard<'a> {
        let mut guard = self.lock();
        guard.retired = Some(retired);
        guard
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
        let existing = self
            .root_dir
            .lock_irqsave()
            .entry
            .as_ref()
            .and_then(WeakDirEntry::upgrade);
        if let Some(root) = existing {
            return root;
        }
        let holder = self.root_holder();
        let candidate = DirEntry::new_dir(
            |this| FatDirNode::from_state(holder, this),
            Reference::root(),
        );
        let (root, retired) = {
            let mut index = self.root_dir.lock_irqsave();
            match index.entry.as_ref().and_then(WeakDirEntry::upgrade) {
                Some(root) => (root, None),
                None => {
                    let retired = index.entry.replace(candidate.downgrade());
                    (candidate.clone(), retired)
                }
            }
        };
        // A losing candidate owns a raw holder whose destructor takes the
        // filesystem mutex. Both index locks and the mutex are gone here.
        drop(candidate);
        drop(retired);
        root
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
        crate::file::sync_fat_cached_files(self)?;
        let _state = self.inner.lock();
        self.disk_flusher
            .flush()
            .map_err(crate::block_error_to_vfs_error)
    }

    fn shutdown(&self) -> VfsResult<()> {
        self.flush()
    }
}
