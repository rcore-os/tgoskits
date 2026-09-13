use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
};

mod access;
mod admission;
mod directory;
mod guard;
mod lifetime;
mod mmp;
mod mutation;
mod namespace;
mod read;
mod read_cache;
mod worker;
mod write;
mod writeback;
pub(crate) use access::AccessGate;
use admission::Admission;
use axfs_ng_vfs::{
    DirEntry, DirNode, Filesystem, FilesystemMountLease, FilesystemOps, Reference, StatFs,
    VfsError, VfsResult, WeakDirEntry, path::MAX_NAME_LEN,
};
pub(crate) use guard::Ext4Guard;
pub(crate) use lifetime::{InodeLifetime, LocatedInode};
use mmp::mmp_identity_for_region;
use namespace::Namespace;
pub(crate) use namespace::NamespaceChange;
use rsext4::{InodeNumber, MmpIdentity, MountServices};
use worker::MountWorker;
use writeback::Writeback;

use super::{
    Ext4Clock, Ext4Delay, Ext4Disk, Ext4Entropy, Ext4Observer, Inode, MountedExt4,
    util::into_vfs_err,
};
use crate::{
    block::{BlockRegion, FsBlockDevice},
    os::sync::SleepMutex as Mutex,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReapClaim(InodeNumber);

#[derive(Default)]
struct InodeLifetimeTracker {
    live_refs: BTreeMap<InodeNumber, usize>,
    zero_link: BTreeSet<InodeNumber>,
    reaping: BTreeSet<InodeNumber>,
}

impl InodeLifetimeTracker {
    fn inc_ref(&mut self, inode: InodeNumber) {
        self.live_refs
            .entry(inode)
            .and_modify(|count| *count += 1)
            .or_insert(1);
    }

    fn claim_if_ready(&mut self, inode: InodeNumber) -> Option<ReapClaim> {
        (!self.live_refs.contains_key(&inode)
            && self.zero_link.contains(&inode)
            && self.reaping.insert(inode))
        .then_some(ReapClaim(inode))
    }

    fn publish_zero_link(&mut self, inode: InodeNumber) -> Option<ReapClaim> {
        self.zero_link.insert(inode);
        self.claim_if_ready(inode)
    }

    fn release_ref(&mut self, inode: InodeNumber) -> Option<ReapClaim> {
        use alloc::collections::btree_map::Entry;

        let became_unreferenced = match self.live_refs.entry(inode) {
            Entry::Occupied(mut entry) => {
                let count = entry.get_mut();
                *count = count.saturating_sub(1);
                if *count == 0 {
                    entry.remove();
                    true
                } else {
                    false
                }
            }
            Entry::Vacant(_) => false,
        };
        became_unreferenced
            .then(|| self.claim_if_ready(inode))
            .flatten()
    }

    fn finish_reap(&mut self, claim: ReapClaim, succeeded: bool) {
        self.reaping.remove(&claim.0);
        if succeeded {
            self.zero_link.remove(&claim.0);
        }
    }

    fn claim_pending_reap(&mut self) -> Option<ReapClaim> {
        let inode =
            self.zero_link.iter().copied().find(|inode| {
                !self.live_refs.contains_key(inode) && !self.reaping.contains(inode)
            })?;
        self.reaping.insert(inode);
        Some(ReapClaim(inode))
    }

    fn has_pending_reaps(&self) -> bool {
        !self.zero_link.is_empty()
    }
}

pub(crate) struct Ext4State {
    pub ext4: MountedExt4,
    lifetimes: InodeLifetimeTracker,
    shutdown_attempted: bool,
    dirty: bool,
    staging: bool,
}

impl Ext4State {
    fn unmount(&mut self) -> VfsResult<()> {
        // An uncertain final MMP CLEAN write must not be retried from Drop.
        self.shutdown_attempted = true;
        self.ext4.unmount().map_err(into_vfs_err)
    }

    pub(crate) fn release_ref(&mut self, ino: InodeNumber) -> Option<ReapClaim> {
        self.lifetimes.release_ref(ino)
    }

    pub(crate) fn publish_zero_link(&mut self, ino: InodeNumber) -> Option<ReapClaim> {
        self.lifetimes.publish_zero_link(ino)
    }

    fn finish_reap(&mut self, claim: ReapClaim, succeeded: bool) {
        self.lifetimes.finish_reap(claim, succeeded);
    }

    fn has_pending_reaps(&self) -> bool {
        self.lifetimes.has_pending_reaps()
    }

    fn claim_pending_reap(&mut self) -> Option<ReapClaim> {
        self.lifetimes.claim_pending_reap()
    }
}

pub struct Ext4Filesystem {
    self_ref: Weak<Self>,
    inner: Mutex<Ext4State>,
    inode_metadata: rsext4::InodeMetadataReader,
    mmp_worker: MountWorker,
    writeback: Writeback,
    inode_access: Mutex<BTreeMap<InodeNumber, Weak<AccessGate>>>,
    namespace: Namespace,
    admission: Admission,
    cached_writes: Admission,
    root_dir: Mutex<Option<WeakDirEntry>>,
    mount_lease: Mutex<Weak<Ext4MountLease>>,
}

struct Ext4MountLease {
    filesystem: Arc<Ext4Filesystem>,
}

impl core::fmt::Debug for Ext4MountLease {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Ext4MountLease")
    }
}

impl FilesystemMountLease for Ext4MountLease {}

impl Drop for Ext4MountLease {
    fn drop(&mut self) {
        let lease = self.filesystem.mount_lease.lock();
        // A new generation may have been published before this destructor
        // acquired exclusion. In that case it owns the still-live caches.
        if lease.strong_count() != 0 {
            return;
        }
        if let Err(error) = crate::file::retire_filesystem_cache(&*self.filesystem) {
            log::error!("failed to retire final ext4 mount cache: {error:?}");
            // Without the high-level VFS registry, the dentry tree may be
            // the last owner of dirty cache pages. Retain it on failure.
            #[cfg(not(feature = "vfs"))]
            return;
        }
        // Cached children strongly reference their parents. Clear the tree
        // only after the last mount (including bind aliases) is gone.
        let root = self
            .filesystem
            .root_dir
            .lock()
            .as_ref()
            .and_then(WeakDirEntry::upgrade);
        if let Some(root) = root {
            root.as_dir()
                .expect("filesystem root directory")
                .clear_cached_entries();
        }
    }
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
        let mmp_identity = mmp_identity_for_region(dev.name(), region);
        let disk = Ext4Disk::new(dev, region).map_err(into_vfs_err)?;
        let services = MountServices::new(Ext4Clock, Ext4Entropy, Ext4Observer)
            .with_mmp(Ext4Delay, mmp_identity);
        let mut ext4 =
            rsext4::Ext4::mount_with_readonly_fallback(disk, services).map_err(into_vfs_err)?;
        if ext4.options().readonly {
            warn!("ext4 recovery required a read-only fallback mount");
        }
        let background_writeback = Self::configure_writeback(&mut ext4)?;
        let inode_metadata = ext4.inode_metadata_reader();
        let fs = Arc::new_cyclic(|self_ref| Self {
            self_ref: self_ref.clone(),
            inode_metadata,
            inner: Mutex::new(Ext4State {
                ext4,
                lifetimes: InodeLifetimeTracker::default(),
                shutdown_attempted: false,
                dirty: false,
                staging: false,
            }),
            mmp_worker: MountWorker::disabled(),
            writeback: Writeback::new(background_writeback),
            inode_access: Mutex::new(BTreeMap::new()),
            namespace: Namespace::new(),
            admission: Admission::new(),
            cached_writes: Admission::new(),
            root_dir: Mutex::new(None),
            mount_lease: Mutex::new(Weak::new()),
        });
        if fs.lock().ext4.mmp_refresh_interval().is_some()
            && let Err(error) = fs.start_mmp_worker()
        {
            if background_writeback {
                fs.lock()
                    .ext4
                    .disable_background_writeback()
                    .map_err(into_vfs_err)?;
            }
            let cleanup = fs.lock().unmount();
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
        if let Err(error) = fs.start_writeback_worker() {
            fs.mmp_worker.stop_and_join();
            fs.lock()
                .ext4
                .disable_background_writeback()
                .map_err(into_vfs_err)?;
            let cleanup = fs.lock().unmount();
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
        Ok(Filesystem::new(fs))
    }

    /// Locks the shared rsext4 state.
    ///
    /// Uses a blocking mutex because rsext4 operations may issue block I/O while
    /// this guard is held. IRQ-driven block submission sleeps until the
    /// maintenance thread publishes completion, so the outer filesystem state
    /// guard must not disable interrupts or preemption.
    pub(crate) fn lock(&self) -> Ext4Guard<'_> {
        Ext4Guard::acquire(&self.inner, &self.cached_writes)
    }

    pub(crate) fn block_size(&self) -> u64 {
        u64::from(self.inode_metadata.block_size())
    }

    pub(crate) fn inode_access(&self, inode: InodeNumber) -> Arc<AccessGate> {
        // Registry lookup may nest under ext4 state, but never waits for an
        // inode lock. Content paths take inode exclusion before ext4 state.
        let mut locks = self.inode_access.lock();
        if let Some(lock) = locks.get(&inode).and_then(Weak::upgrade) {
            return lock;
        }
        locks.retain(|_, lock| lock.strong_count() != 0);
        let lock = Arc::new(AccessGate::new());
        locks.insert(inode, Arc::downgrade(&lock));
        lock
    }

    pub(crate) fn reap(&self, claim: ReapClaim) -> VfsResult<()> {
        let _operation = match self.admission.enter() {
            Ok(operation) => operation,
            Err(error) => {
                // A final inode drop can race shutdown after normal writers
                // have drained. Return its claim to the shutdown owner rather
                // than modifying metadata behind the closed admission gate.
                self.lock().finish_reap(claim, false);
                return Err(into_vfs_err(error));
            }
        };
        self.reap_admitted(claim)
    }

    /// Called with operation admission or by the exclusive shutdown owner.
    fn reap_admitted(&self, claim: ReapClaim) -> VfsResult<()> {
        // Reap continues across pressure after unlink has already published
        // the orphan. The lifetime claim excludes a second reaper.
        let result = loop {
            let result = {
                let mut state = self.lock();
                state.dirty = true;
                if state.staging {
                    None
                } else {
                    // The lifetime claim proves that no FileNode (including
                    // cached backing owners) still references this inode.
                    // Remove its weak key before allocation may reuse it.
                    crate::file::forget_cached_file_key(self, claim.0.as_u64());
                    Some(state.ext4.reap_unlinked_inode(claim.0))
                }
            };
            match result {
                None => {
                    if let Err(error) = self.sync_core_for_reap() {
                        break Err(error);
                    }
                }
                Some(Err(error)) if error.requires_journal_progress() => {
                    if let Err(error) = self.sync_core_for_reap() {
                        break Err(error);
                    }
                }
                Some(result) => break result,
            }
        };
        self.lock().finish_reap(claim, result.is_ok());
        result.map_err(into_vfs_err)
    }

    fn shutdown_filesystem(&self) -> VfsResult<()> {
        self.cached_writes.close_and_drain().map_err(into_vfs_err)?;
        let result = self.shutdown_after_cached_writes();
        if result.is_err() && !self.lock().shutdown_attempted {
            if self.lock().ext4.mmp_refresh_interval().is_some()
                && let Err(error) = self.start_mmp_worker()
            {
                log::error!("ext4 MMP restart after failed shutdown: {error:?}");
                self.lock().ext4.report_mmp_runtime_failure(
                    rsext4::Ext4Error::unsupported_capability("runtime:mmp_worker"),
                );
                return result;
            }
            if let Err(error) = self.start_writeback_worker() {
                log::error!("ext4 writeback restart after failed shutdown: {error:?}");
                return result;
            }
            // Reopen only after both required workers are live again. A
            // failed restart must not admit writes without their drain owner.
            self.admission.reopen();
            self.cached_writes.reopen();
        }
        result
    }

    fn shutdown_after_cached_writes(&self) -> VfsResult<()> {
        // The worker may already own a page snapshot. Join it before the
        // final pass, outside filesystem, registry and file I/O locks.
        self.writeback.stop_and_join();
        crate::file::writeback_filesystem_pages(self)?;
        self.admission.close_and_drain().map_err(into_vfs_err)?;
        self.shutdown_quiescent()
    }

    fn shutdown_quiescent(&self) -> VfsResult<()> {
        loop {
            let claim = self.lock().claim_pending_reap();
            let Some(claim) = claim else {
                break;
            };
            self.reap_admitted(claim)?;
        }
        if self.lock().has_pending_reaps() {
            return Err(into_vfs_err(rsext4::Ext4Error::busy()));
        }
        self.mmp_worker.stop_and_join();
        self.writeback.stop_and_join();
        self.unmount_after_drain()
    }
}

impl Drop for Ext4Filesystem {
    fn drop(&mut self) {
        self.writeback.stop();
        // The MMP worker may be dropping the final strong reference itself.
        // Stop and notify it, but never join the current worker from Drop.
        self.mmp_worker.stop();
        let attempted = self.lock().shutdown_attempted;
        if !attempted && let Err(error) = self.unmount_after_drain() {
            log::error!("failed to unmount the final ext4 reference: {error:?}");
        }
    }
}

impl FilesystemOps for Ext4Filesystem {
    fn name(&self) -> &str {
        "ext4"
    }

    fn is_readonly(&self) -> bool {
        self.lock().ext4.options().readonly
    }

    fn cached_write_admission(&self) -> Option<&dyn axfs_ng_vfs::CachedWriteAdmission> {
        Some(&self.cached_writes)
    }

    fn mount_lease(&self) -> Option<Arc<dyn FilesystemMountLease>> {
        let mut installed = self.mount_lease.lock();
        if let Some(lease) = installed.upgrade() {
            return Some(lease);
        }
        let lease = Arc::new(Ext4MountLease {
            filesystem: self.self_ref.upgrade().expect("live filesystem owner"),
        });
        *installed = Arc::downgrade(&lease);
        Some(lease)
    }

    fn root_dir(&self) -> DirEntry {
        let mut root = self.root_dir.lock();
        if let Some(entry) = root.as_ref().and_then(WeakDirEntry::upgrade) {
            return entry;
        }

        // Inodes own the filesystem. Keeping only a weak root here avoids
        // the filesystem -> root inode -> filesystem ownership cycle.
        let filesystem = self.self_ref.upgrade().expect("live filesystem owner");
        let lifetime = {
            let mut state = self.lock();
            let root_ino = state.ext4.root_inode();
            state.retain_inode(&filesystem, root_ino)
        };
        let entry = DirEntry::new_dir(
            |this| DirNode::new(Inode::new(lifetime, Some(this))),
            Reference::root(),
        );
        *root = Some(entry.downgrade());
        entry
    }

    fn stat(&self) -> VfsResult<StatFs> {
        let state = self.lock();
        let stats = state.ext4.statfs();
        Ok(StatFs {
            fs_type: 0xef53,
            block_size: stats.block_size as _,
            blocks: stats.total_blocks,
            blocks_free: stats.free_blocks,
            blocks_available: stats.free_blocks,
            file_count: stats.total_inodes as _,
            free_file_count: stats.free_inodes as _,
            name_length: MAX_NAME_LEN as _,
            fragment_size: 0,
            mount_flags: 0,
        })
    }

    fn flush(&self) -> VfsResult<()> {
        self.sync_to_disk()
    }

    fn shutdown(&self) -> VfsResult<()> {
        self.shutdown_filesystem()
    }
}

#[cfg(test)]
mod tests;
