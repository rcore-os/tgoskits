use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use core::cell::OnceCell;

use axfs_ng_vfs::{
    DirEntry, DirNode, Filesystem, FilesystemOps, Reference, StatFs, VfsResult, path::MAX_NAME_LEN,
};
use rsext4::{Jbd2Dev, MountOptions, bmalloc::InodeNumber};

use super::{Ext4Disk, Ext4Observer, Inode, util::into_vfs_err};
use crate::{
    block::{BlockRegion, FsBlockDevice},
    os::sync::{SleepMutex as Mutex, SleepMutexGuard as MutexGuard},
};

const EXT4_ROOT_INO: u32 = 2;

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
    pub fs: rsext4::Ext4FileSystem,
    pub dev: Jbd2Dev<Ext4Disk>,
    lifetimes: InodeLifetimeTracker,
}

impl Ext4State {
    pub(crate) fn split(&mut self) -> (&mut rsext4::Ext4FileSystem, &mut Jbd2Dev<Ext4Disk>) {
        let fs = &mut self.fs as *mut _;
        let dev = &mut self.dev as *mut _;
        unsafe { (&mut *fs, &mut *dev) }
    }

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
                    "ext4 filesystem is in error state; replaying journal then mounting read-only"
                );
                Self::mount_readonly_fallback(dev, true)?
            }
            Ok(false) => match rsext4::mount_with_options_and_observer(
                &mut dev,
                MountOptions::read_write(),
                &mut Ext4Observer,
            ) {
                Ok(fs) => (fs, dev, false),
                Err(err) if err.is_corruption() => {
                    warn!(
                        "ext4 journal replay failed with EUCLEAN; retrying read-only without \
                         journal replay"
                    );
                    Self::mount_readonly_fallback(dev, false)?
                }
                Err(err) => return Err(into_vfs_err(err)),
            },
            Err(err) if err.is_corruption() => {
                warn!(
                    "ext4 superblock check failed with EUCLEAN; retrying read-only without \
                     journal replay"
                );
                Self::mount_readonly_fallback(dev, false)?
            }
            Err(err) => return Err(into_vfs_err(err)),
        };

        let fs = Arc::new(Self {
            inner: Mutex::new(Ext4State {
                fs,
                dev,
                lifetimes: InodeLifetimeTracker::default(),
            }),
            root_dir: OnceCell::new(),
            readonly,
        });
        let root_ino = InodeNumber::new(EXT4_ROOT_INO).unwrap();
        fs.lock().inc_ref(root_ino);
        let _ = fs.root_dir.set(DirEntry::new_dir(
            |this| {
                DirNode::new(Inode::new(
                    fs.clone(),
                    root_ino,
                    Some(this),
                    Some("/".into()),
                ))
            },
            Reference::root(),
        ));
        Ok(Filesystem::new(fs))
    }

    /// Mount read-only as a fallback when journal replay fails or the
    /// filesystem is in error state.
    ///
    /// Linux always replays the journal before mounting read-only when the
    /// filesystem is in error state, because unreplayed journal transactions
    /// leave metadata inconsistent.  We mirror that behaviour.
    ///
    /// Crucially, we never call `into_inner()` on the `Jbd2Dev` — the block
    /// cache must be preserved so that reads after mount (e.g. loading a
    /// guest kernel image) can hit the cache rather than issuing fresh
    /// hardware I/O that may hang on a controller left in a bad state by the
    /// failed mount attempt.
    fn mount_readonly_fallback(
        mut dev: Jbd2Dev<Ext4Disk>,
        replay_journal: bool,
    ) -> VfsResult<(rsext4::Ext4FileSystem, Jbd2Dev<Ext4Disk>, bool)> {
        let fs = rsext4::mount_with_options_and_observer(
            &mut dev,
            MountOptions {
                readonly: true,
                replay_journal,
            },
            &mut Ext4Observer,
        )
        .map_err(into_vfs_err)?;
        Ok((fs, dev, true))
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
        let (fs, dev) = state.split();
        fs.sync_filesystem_with_observer(dev, &mut Ext4Observer)
            .map_err(into_vfs_err)?;
        if dev.is_use_journal() {
            dev.umount_commit().map_err(into_vfs_err)?;
        }
        dev.cantflush().map_err(into_vfs_err)
    }

    pub(crate) fn reap(&self, claim: ReapClaim) -> VfsResult<()> {
        let mut state = self.inner.lock();
        let result = {
            let (fs, dev) = state.split();
            rsext4::reap_unlinked_inode(fs, dev, claim.0).map_err(into_vfs_err)
        };
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
        let (fs, dev) = state.split();
        fs.umount_with_observer(dev, &mut Ext4Observer)
            .map_err(into_vfs_err)
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

    fn shutdown(&self) -> VfsResult<()> {
        self.shutdown_filesystem()
    }
}
