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
#[cfg(feature = "vfs")]
use crate::block::file_image::FileBackingLease;
use crate::{
    block::{BlockRegion, FsBlockDevice},
    os::sync::Mutex,
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

    pub(crate) fn has_pending_reaps(&self) -> bool {
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
    mount_lease: Mutex<MountLeaseState>,
    shutdown_state: Mutex<ShutdownState>,
    #[cfg(feature = "vfs")]
    backing_lease: Option<Arc<dyn FileBackingLease>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum LeasePhase {
    #[default]
    Active,
    #[cfg(feature = "vfs")]
    Retiring,
    #[cfg(feature = "vfs")]
    Terminal,
}

#[derive(Default)]
struct MountLeaseState {
    installed: Weak<Ext4MountLease>,
    generation: u64,
    phase: LeasePhase,
}

#[derive(Default)]
struct ShutdownState {
    complete: bool,
}

/// Selects whether the filesystem owns a removable file backend. Physical
/// root filesystems keep their historical cache-only lease retirement.
struct Ext4Backing {
    #[cfg(feature = "vfs")]
    lease: Option<Arc<dyn FileBackingLease>>,
}

impl Ext4Backing {
    fn physical() -> Self {
        Self {
            #[cfg(feature = "vfs")]
            lease: None,
        }
    }

    #[cfg(feature = "vfs")]
    fn file(lease: Arc<dyn FileBackingLease>) -> Self {
        Self { lease: Some(lease) }
    }
}

struct Ext4MountLease {
    filesystem: Arc<Ext4Filesystem>,
    generation: u64,
}

impl core::fmt::Debug for Ext4MountLease {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Ext4MountLease")
    }
}

impl FilesystemMountLease for Ext4MountLease {}

impl Drop for Ext4MountLease {
    fn drop(&mut self) {
        #[cfg(feature = "vfs")]
        if self.filesystem.backing_lease.is_some() {
            self.retire_file_backed_mount();
            return;
        }

        // Physical root filesystems keep the historical behavior: the cached
        // tree is retired while the lease lock excludes a new generation.
        // Unlike the file-backed path below, this path performs no shutdown or
        // worker join, so retaining the existing lock scope is safe.
        let state = self.filesystem.mount_lease.lock();
        if state.phase != LeasePhase::Active
            || state.generation != self.generation
            || state.installed.strong_count() != 0
        {
            return;
        }
        let _ = self.filesystem.retire_mount_caches();
        drop(state);
    }
}

#[cfg(feature = "vfs")]
impl Ext4MountLease {
    fn retire_file_backed_mount(&self) {
        if !self.claim_final_retirement() {
            return;
        }

        // Serialize retirement with an explicit `FilesystemOps::shutdown`
        // caller. The mount-lease lock is intentionally not held here: cache
        // writeback, worker joins and device I/O may sleep or re-enter.
        let mut shutdown = self.filesystem.shutdown_state.lock();
        if self.filesystem.retire_mount_caches().is_err() {
            drop(shutdown);
            self.finish_file_backed_retirement();
            return;
        }
        if !shutdown.complete {
            match self.filesystem.shutdown_filesystem_locked() {
                Ok(()) => shutdown.complete = true,
                Err(error) => {
                    log::error!("failed to shut down the final file-backed ext4 mount: {error:?}");
                    drop(shutdown);
                    self.finish_file_backed_retirement();
                    return;
                }
            }
        }
        drop(shutdown);

        if let Some(backing) = &self.filesystem.backing_lease {
            backing.release();
        }
        self.finish_file_backed_retirement();
    }

    /// Claims the last lease generation atomically. Publishing `Retiring`
    /// under the same lock rejects a racing `mount_lease` call, while the
    /// generation check rejects any lease installed before this destructor
    /// acquired the lock.
    fn claim_final_retirement(&self) -> bool {
        let mut state = self.filesystem.mount_lease.lock();
        if state.phase != LeasePhase::Active
            || state.generation != self.generation
            || state.installed.strong_count() != 0
        {
            return false;
        }
        state.installed = Weak::new();
        state.phase = LeasePhase::Retiring;
        true
    }

    fn finish_file_backed_retirement(&self) {
        let mut state = self.filesystem.mount_lease.lock();
        debug_assert_eq!(state.phase, LeasePhase::Retiring);
        state.phase = LeasePhase::Terminal;
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
        Self::new_from_boxed_inner(dev, region, Ext4Backing::physical())
    }

    #[cfg(feature = "vfs")]
    pub(crate) fn new_from_boxed_with_backing(
        dev: Box<dyn FsBlockDevice>,
        region: BlockRegion,
        lease: Arc<dyn FileBackingLease>,
    ) -> VfsResult<Filesystem> {
        Self::new_from_boxed_inner(dev, region, Ext4Backing::file(lease))
    }

    fn new_from_boxed_inner(
        dev: Box<dyn FsBlockDevice>,
        region: BlockRegion,
        _backing: Ext4Backing,
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
            mount_lease: Mutex::new(MountLeaseState::default()),
            shutdown_state: Mutex::new(ShutdownState::default()),
            #[cfg(feature = "vfs")]
            backing_lease: _backing.lease,
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

    pub(crate) fn directory_cache_generation(&self, inode: InodeNumber) -> VfsResult<Option<u64>> {
        // Namespace commits advance the canonical parent inode change attribute
        // through rsext4's parent_dir_change, invalidating misses in every
        // renamed/open directory alias. A fixed or wrapper-local version would
        // keep stale misses after another alias creates a name.
        // The read-only capability never loads or waits; paused, evicted and
        // retired metadata must force ordinary lookup.
        self.inode_metadata
            .try_get(inode)
            .map(|cached| cached.map(|info| info.change_attribute))
            .map_err(into_vfs_err)
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

    /// Retires the global cache registry and the cached dentry tree. Cached
    /// children strongly reference their parents, so the tree is cleared only
    /// after the last mount (including bind aliases) is gone.
    fn retire_mount_caches(&self) -> VfsResult<()> {
        let result = crate::file::retire_filesystem_cache(self);
        if let Err(error) = &result {
            log::error!("failed to retire final ext4 mount cache: {error:?}");
            // Without the global writeback registry, dentries are the only
            // remaining dirty-cache owners. Preserve them after an I/O error.
            #[cfg(not(feature = "vfs"))]
            return result;
        }
        let root = self
            .root_dir
            .lock()
            .as_ref()
            .and_then(WeakDirEntry::upgrade);
        if let Some(root) = root {
            root.as_dir()
                .expect("filesystem root directory")
                .clear_cached_entries();
        }
        result
    }

    /// Shuts down exactly once. Concurrent explicit shutdowns and final mount
    /// retirement serialize here; a completed shutdown is idempotent.
    fn shutdown_filesystem(&self) -> VfsResult<()> {
        let mut shutdown = self.shutdown_state.lock();
        if shutdown.complete {
            return Ok(());
        }
        let result = self.shutdown_filesystem_locked();
        if result.is_ok() {
            shutdown.complete = true;
        }
        result
    }

    /// Caller holds `shutdown_state`, so a second owner cannot re-enter the
    /// admission drain while the first is still restarting or joining workers.
    fn shutdown_filesystem_locked(&self) -> VfsResult<()> {
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
        let mut state = self.mount_lease.lock();
        // Final retirement owns the transition. Do not publish a lease for a
        // retiring instance. A final retirement attempt marks the instance
        // terminal whether shutdown succeeds or the backing guard is retained
        // after failure. A caller that still builds a Mountpoint from a
        // retained Filesystem handle receives no lifetime lease. After
        // successful shutdown the closed admission gate rejects its I/O before
        // the released device; after failure the retained guard remains owned
        // by the filesystem until the RAII fallback runs.
        if state.phase != LeasePhase::Active {
            return None;
        }
        if let Some(lease) = state.installed.upgrade() {
            return Some(lease);
        }
        state.generation = state.generation.wrapping_add(1);
        let lease = Arc::new(Ext4MountLease {
            filesystem: self.self_ref.upgrade().expect("live filesystem owner"),
            generation: state.generation,
        });
        state.installed = Arc::downgrade(&lease);
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
