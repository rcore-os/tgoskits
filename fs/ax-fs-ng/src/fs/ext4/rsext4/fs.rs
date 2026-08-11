use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use ax_lazyinit::LazyInit;
use axfs_ng_vfs::{
    DirEntry, DirNode, Filesystem, FilesystemOps, Reference, StatFs, VfsResult, path::MAX_NAME_LEN,
};
use rsext4::{MountServices, bmalloc::InodeNumber};

use super::{Ext4Clock, Ext4Disk, Ext4Observer, Inode, MountedExt4, util::into_vfs_err};
use crate::{
    block::{BlockRegion, FsBlockDevice},
    os::sync::{SleepMutex as Mutex, SleepMutexGuard as MutexGuard},
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

    fn has_pending_reaps(&self) -> bool {
        !self.zero_link.is_empty()
    }
}

pub(crate) struct Ext4State {
    pub ext4: MountedExt4,
    lifetimes: InodeLifetimeTracker,
}

impl Ext4State {
    pub(crate) fn inc_ref(&mut self, ino: InodeNumber) {
        self.lifetimes.inc_ref(ino);
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
}

pub struct Ext4Filesystem {
    inner: Mutex<Ext4State>,
    root_dir: LazyInit<DirEntry>,
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
        let services = MountServices::new(Ext4Clock, (), (), (), Ext4Observer);
        let ext4 =
            rsext4::Ext4::mount_with_readonly_fallback(disk, services).map_err(into_vfs_err)?;
        let readonly = ext4.options().readonly;
        if readonly {
            warn!("ext4 recovery required a read-only fallback mount");
        }
        let root_ino = ext4.root_inode();

        let fs = Arc::new(Self {
            inner: Mutex::new(Ext4State {
                ext4,
                lifetimes: InodeLifetimeTracker::default(),
            }),
            root_dir: LazyInit::new(),
            readonly,
        });
        fs.lock().inc_ref(root_ino);
        fs.root_dir.init_once(DirEntry::new_dir(
            |this| DirNode::new(Inode::new(fs.clone(), root_ino, Some(this))),
            Reference::root(),
        ));
        Ok(Filesystem::new(fs))
    }

    /// Locks the shared rsext4 state.
    ///
    /// Uses a blocking mutex because rsext4 operations may issue block I/O while
    /// this guard is held. IRQ-driven block submission sleeps until the
    /// maintenance thread publishes completion, so the outer filesystem state
    /// guard must not disable interrupts or preemption.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Ext4State> {
        self.inner.lock()
    }

    pub(crate) fn sync_to_disk(&self) -> VfsResult<()> {
        if self.readonly {
            return Ok(());
        }

        let mut state = self.inner.lock();
        state.ext4.sync().map_err(into_vfs_err)
    }

    pub(crate) fn reap(&self, claim: ReapClaim) -> VfsResult<()> {
        let mut state = self.inner.lock();
        let result = state
            .ext4
            .reap_unlinked_inode(claim.0)
            .map_err(into_vfs_err);
        state.finish_reap(claim, result.is_ok());
        result
    }

    fn shutdown_filesystem(&self) -> VfsResult<()> {
        if self.readonly {
            return Ok(());
        }

        let mut state = self.inner.lock();
        if state.has_pending_reaps() {
            return Err(into_vfs_err(rsext4::Ext4Error::busy()));
        }
        state.ext4.unmount().map_err(into_vfs_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_link_reap_claim_is_unique_and_retryable() {
        let inode = InodeNumber::new(42).unwrap();
        let mut tracker = InodeLifetimeTracker::default();
        tracker.inc_ref(inode);
        tracker.inc_ref(inode);

        assert_eq!(tracker.publish_zero_link(inode), None);
        assert_eq!(tracker.release_ref(inode), None);
        let claim = tracker
            .release_ref(inode)
            .expect("last ref must claim reap");
        assert_eq!(tracker.claim_if_ready(inode), None);

        tracker.finish_reap(claim, false);
        let retry = tracker
            .claim_if_ready(inode)
            .expect("failed reap must remain retryable");
        tracker.finish_reap(retry, true);
        assert!(!tracker.has_pending_reaps());
    }
}

impl FilesystemOps for Ext4Filesystem {
    fn name(&self) -> &str {
        "ext4"
    }

    fn is_readonly(&self) -> bool {
        self.readonly
    }

    fn root_dir(&self) -> DirEntry {
        self.root_dir.clone()
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
