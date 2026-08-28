use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_lazyinit::LazyInit;
use axfs_ng_vfs::{
    DirEntry, DirNode, Filesystem, FilesystemOps, Reference, StatFs, VfsError, VfsResult,
    path::MAX_NAME_LEN,
};
use rsext4::{InodeNumber, MmpIdentity, MountServices};

use super::{
    Ext4Clock, Ext4Delay, Ext4Disk, Ext4Entropy, Ext4Observer, Inode, MountedExt4,
    util::into_vfs_err,
};
use crate::{
    block::{BlockRegion, FsBlockDevice},
    block_error_to_vfs_error,
    os::{
        BlockNotification, BlockThread, runtime_ops,
        sync::{IrqMutex, SleepMutex as Mutex, SleepMutexGuard as MutexGuard},
    },
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

    fn claim_pending_reap(&mut self) -> Option<ReapClaim> {
        self.lifetimes.claim_pending_reap()
    }
}

pub struct Ext4Filesystem {
    self_ref: Weak<Self>,
    inner: Mutex<Ext4State>,
    mmp_worker: MmpWorker,
    root_dir: LazyInit<DirEntry>,
}

struct MmpWorker {
    stopping: Arc<AtomicBool>,
    notification: IrqMutex<Option<Arc<dyn BlockNotification>>>,
    thread: IrqMutex<Option<Box<dyn BlockThread>>>,
}

impl MmpWorker {
    fn disabled() -> Self {
        Self {
            stopping: Arc::new(AtomicBool::new(false)),
            notification: IrqMutex::new(None),
            thread: IrqMutex::new(None),
        }
    }

    fn stop_and_join(&self) {
        self.stopping.store(true, Ordering::Release);
        let notification = self.notification.lock().clone();
        if let Some(notification) = notification {
            notification.notify();
        }
        let thread = self.thread.lock().take();
        if let Some(thread) = thread {
            thread.join();
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
        let ext4 =
            rsext4::Ext4::mount_with_readonly_fallback(disk, services).map_err(into_vfs_err)?;
        if ext4.options().readonly {
            warn!("ext4 recovery required a read-only fallback mount");
        }
        let root_ino = ext4.root_inode();

        let fs = Arc::new_cyclic(|self_ref| Self {
            self_ref: self_ref.clone(),
            inner: Mutex::new(Ext4State {
                ext4,
                lifetimes: InodeLifetimeTracker::default(),
            }),
            mmp_worker: MmpWorker::disabled(),
            root_dir: LazyInit::new(),
        });
        if fs.lock().ext4.mmp_refresh_interval().is_some()
            && let Err(error) = fs.start_mmp_worker()
        {
            let cleanup = fs.lock().ext4.unmount().map_err(into_vfs_err);
            return match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(cleanup_error),
            };
        }
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
        loop {
            let claim = self.inner.lock().claim_pending_reap();
            let Some(claim) = claim else {
                break;
            };
            self.reap(claim)?;
        }
        if self.inner.lock().has_pending_reaps() {
            return Err(into_vfs_err(rsext4::Ext4Error::busy()));
        }
        self.mmp_worker.stop_and_join();
        let result = self.inner.lock().ext4.unmount().map_err(into_vfs_err);
        if result.is_err()
            && self.inner.lock().ext4.mmp_refresh_interval().is_some()
            && let Err(worker_error) = self.start_mmp_worker()
        {
            self.inner.lock().ext4.report_mmp_runtime_failure(
                rsext4::Ext4Error::unsupported_capability("runtime:mmp_worker"),
            );
            return Err(worker_error);
        }
        result
    }

    fn start_mmp_worker(&self) -> VfsResult<()> {
        if self.mmp_worker.thread.lock().is_some() {
            return Ok(());
        }
        let runtime = runtime_ops().map_err(block_error_to_vfs_error)?;
        let notification = runtime.notification();
        if self.self_ref.upgrade().is_none() {
            return Err(VfsError::BadState);
        }
        let worker_fs = self.self_ref.clone();
        let worker_notification = Arc::clone(&notification);
        let stopping = Arc::clone(&self.mmp_worker.stopping);
        stopping.store(false, Ordering::Release);
        let cpu = runtime.current_cpu();
        let thread = runtime
            .spawn_pinned(
                String::from("ext4-mmp"),
                cpu,
                Box::new(move || {
                    run_mmp_worker(worker_fs, worker_notification, stopping);
                }),
            )
            .map_err(block_error_to_vfs_error)?;

        *self.mmp_worker.notification.lock() = Some(notification);
        *self.mmp_worker.thread.lock() = Some(thread);
        Ok(())
    }
}

fn mmp_identity_for_region(device_name: &str, region: BlockRegion) -> MmpIdentity {
    let region_suffix = alloc::format!("@{:x}", region.start_lba);
    let prefix_len = device_name
        .len()
        .min(32usize.saturating_sub(region_suffix.len()));
    let mut encoded_name = Vec::with_capacity(prefix_len + region_suffix.len());
    encoded_name.extend_from_slice(&device_name.as_bytes()[..prefix_len]);
    encoded_name.extend_from_slice(region_suffix.as_bytes());

    // ax-fs-ng has no global UTS identity. Keep the node field empty and record
    // the concrete device region instead of publishing a process-wide label.
    MmpIdentity::from_names(&[], &encoded_name)
}

fn run_mmp_worker(
    filesystem: Weak<Ext4Filesystem>,
    notification: Arc<dyn BlockNotification>,
    stopping: Arc<AtomicBool>,
) {
    let interval = match filesystem.upgrade() {
        Some(filesystem) => filesystem.lock().ext4.mmp_refresh_interval(),
        None => return,
    };
    let Some(mut interval) = interval else {
        return;
    };
    let mut last_refresh = crate::os::monotonic_time();

    // Linux also runs kmmpd without a delay when a crafted image contains a
    // zero update interval. Do not reinterpret zero as "disable ownership".
    while !stopping.load(Ordering::Acquire) {
        notification.wait_timeout(interval);
        if stopping.load(Ordering::Acquire) {
            break;
        }
        let now = crate::os::monotonic_time();
        let elapsed = now.saturating_sub(last_refresh);
        let Some(filesystem) = filesystem.upgrade() else {
            break;
        };
        let refresh = filesystem.lock().ext4.refresh_mmp(elapsed);
        match refresh {
            Ok(Some(next_interval)) => {
                last_refresh = now;
                interval = next_interval;
            }
            Ok(None) => break,
            Err(error) => {
                log::error!("ext4 MMP refresh failed: {error}");
                break;
            }
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

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use rsext4::{
        EXT4_SUPER_MAGIC, MkfsOptions, SUPERBLOCK_OFFSET, SUPERBLOCK_SIZE, endian::DiskFormat,
        superblock::Ext4Superblock,
    };

    use super::*;
    use crate::{BlockError, BlockResult};

    const TEST_DEVICE_BYTES: usize = 64 * 1024 * 1024;
    const TEST_SECTOR_BYTES: usize = 512;

    struct SharedMemoryDevice {
        storage: Arc<StdMutex<Vec<u8>>>,
        read_only: bool,
        flushes: Arc<AtomicUsize>,
    }

    impl FsBlockDevice for SharedMemoryDevice {
        fn name(&self) -> &str {
            "ext4-readonly-lifecycle-test"
        }

        fn num_blocks(&self) -> u64 {
            (self.storage.lock().unwrap().len() / TEST_SECTOR_BYTES) as u64
        }

        fn block_size(&self) -> usize {
            TEST_SECTOR_BYTES
        }

        fn physical_block_size(&self) -> usize {
            TEST_SECTOR_BYTES
        }

        fn is_read_only(&self) -> bool {
            self.read_only
        }

        fn supports_flush(&self) -> bool {
            true
        }

        fn supports_fua(&self) -> bool {
            false
        }

        fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult {
            let start = usize::try_from(block_id)
                .map_err(|_| BlockError::InvalidRequest)?
                .checked_mul(TEST_SECTOR_BYTES)
                .ok_or(BlockError::InvalidRequest)?;
            let end = start
                .checked_add(buf.len())
                .ok_or(BlockError::InvalidRequest)?;
            let storage = self.storage.lock().unwrap();
            let source = storage.get(start..end).ok_or(BlockError::InvalidRequest)?;
            buf.copy_from_slice(source);
            Ok(())
        }

        fn write_block(&mut self, block_id: u64, buf: &[u8]) -> BlockResult {
            if self.read_only {
                return Err(BlockError::Io);
            }
            let start = usize::try_from(block_id)
                .map_err(|_| BlockError::InvalidRequest)?
                .checked_mul(TEST_SECTOR_BYTES)
                .ok_or(BlockError::InvalidRequest)?;
            let end = start
                .checked_add(buf.len())
                .ok_or(BlockError::InvalidRequest)?;
            let mut storage = self.storage.lock().unwrap();
            let target = storage
                .get_mut(start..end)
                .ok_or(BlockError::InvalidRequest)?;
            target.copy_from_slice(buf);
            Ok(())
        }

        fn write_block_fua(&mut self, _block_id: u64, _buf: &[u8]) -> BlockResult {
            Err(BlockError::Unsupported)
        }

        fn flush(&mut self) -> BlockResult {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn test_filesystem(readonly_fallback: bool) -> (Ext4Filesystem, Arc<AtomicUsize>) {
        let storage = Arc::new(StdMutex::new(alloc::vec![0; TEST_DEVICE_BYTES]));
        let flushes = Arc::new(AtomicUsize::new(0));
        let blocks = (TEST_DEVICE_BYTES / TEST_SECTOR_BYTES) as u64;
        let format_device = SharedMemoryDevice {
            storage: Arc::clone(&storage),
            read_only: false,
            flushes: Arc::clone(&flushes),
        };
        let disk = Ext4Disk::new(
            Box::new(format_device),
            BlockRegion::from_num_blocks(blocks),
        )
        .expect("valid format-device geometry");
        rsext4::format(disk, Ext4Clock, MkfsOptions::default()).expect("format test image");

        if readonly_fallback {
            let mut storage = storage.lock().unwrap();
            let superblock_offset = usize::try_from(SUPERBLOCK_OFFSET).unwrap();
            let superblock_bytes =
                &mut storage[superblock_offset..superblock_offset + SUPERBLOCK_SIZE];
            let mut superblock = Ext4Superblock::from_disk_bytes(superblock_bytes);
            assert_eq!(superblock.s_magic, EXT4_SUPER_MAGIC);
            superblock.s_state |= Ext4Superblock::EXT4_ERROR_FS;
            superblock.update_checksum();
            superblock.to_disk_bytes(superblock_bytes);
        }

        let mount_device = SharedMemoryDevice {
            storage,
            read_only: readonly_fallback,
            flushes: Arc::clone(&flushes),
        };
        let disk = Ext4Disk::new(Box::new(mount_device), BlockRegion::from_num_blocks(blocks))
            .expect("valid mount-device geometry");
        let services = MountServices::new(Ext4Clock, Ext4Entropy, Ext4Observer)
            .with_mmp(Ext4Delay, MmpIdentity::default());
        let ext4 = rsext4::Ext4::mount_with_readonly_fallback(disk, services)
            .expect("mount error-state image read-only");
        assert_eq!(ext4.options().readonly, readonly_fallback);

        (
            Ext4Filesystem {
                self_ref: Weak::new(),
                inner: Mutex::new(Ext4State {
                    ext4,
                    lifetimes: InodeLifetimeTracker::default(),
                }),
                mmp_worker: MmpWorker::disabled(),
                root_dir: LazyInit::new(),
            },
            flushes,
        )
    }

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
            .claim_pending_reap()
            .expect("failed reap must remain retryable");
        tracker.finish_reap(retry, true);
        assert!(!tracker.has_pending_reaps());
    }

    #[test]
    fn mmp_identity_uses_the_device_name_and_region() {
        let whole_device = mmp_identity_for_region("nvme0n1", BlockRegion::new(0, 1024));
        let partition = mmp_identity_for_region("nvme0n1", BlockRegion::new(2048, 1024));

        assert_eq!(whole_device, MmpIdentity::from_names(&[], b"nvme0n1@0"));
        assert_eq!(partition, MmpIdentity::from_names(&[], b"nvme0n1@800"));
        assert_ne!(whole_device, partition);
    }

    #[test]
    fn readonly_sync_preserves_the_core_device_flush_boundary() {
        let (filesystem, flushes) = test_filesystem(true);
        let before = flushes.load(Ordering::Relaxed);

        filesystem.sync_to_disk().expect("sync read-only mount");

        assert_eq!(flushes.load(Ordering::Relaxed), before + 1);
    }

    #[test]
    fn readonly_shutdown_finishes_the_core_mount_lifecycle() {
        let (filesystem, _) = test_filesystem(true);

        filesystem
            .shutdown_filesystem()
            .expect("shutdown read-only mount");

        let mut state = filesystem.lock();
        let options = state.ext4.options();
        let error = state
            .ext4
            .remount(options)
            .expect_err("an unmounted core cannot be remounted in place");
        assert_eq!(error.kind(), rsext4::Ext4ErrorKind::Busy);
        assert_eq!(
            error.context(),
            Some(rsext4::ErrorContext::Operation {
                op: "remount:unmounted"
            })
        );
    }

    #[test]
    fn readonly_query_tracks_core_remount_state() {
        let (filesystem, _) = test_filesystem(false);
        assert!(!filesystem.is_readonly());

        let mut state = filesystem.lock();
        let mut options = state.ext4.options();
        options.readonly = true;
        state.ext4.remount(options).expect("remount read-only");
        drop(state);

        assert!(filesystem.is_readonly());
    }
}
