use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex as StdMutex;

use rsext4::{
    EXT4_SUPER_MAGIC, MkfsOptions, SUPERBLOCK_OFFSET, SUPERBLOCK_SIZE, endian::DiskFormat,
    superblock::Ext4Superblock,
};

use super::*;
use crate::{BlockError, BlockResult};

mod inode_load;
mod inode_writeback;
mod lifetime;
mod namespace;
mod persistence;
mod read;
mod sync_policy;
mod write;
mod writeback;

const TEST_DEVICE_BYTES: usize = 64 * 1024 * 1024;
const TEST_SECTOR_BYTES: usize = 512;

#[derive(Clone)]
struct SharedMemoryDevice {
    storage: Arc<StdMutex<Vec<u8>>>,
    read_only: bool,
    flushes: Arc<AtomicUsize>,
}

impl FsBlockDevice for SharedMemoryDevice {
    fn fork_io(&self) -> BlockResult<Box<dyn FsBlockDevice>> {
        inode_load::observe_fork();
        inode_writeback::check_device_fork()?;
        read::check_device_fork()?;
        write::check_device_fork()?;
        Ok(Box::new(self.clone()))
    }

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
        inode_load::observe_read()?;
        inode_writeback::observe_device_read()?;
        read::observe_device_read(block_id, buf.len())?;
        write::observe_device_read(block_id, buf.len())?;
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
        writeback::observe_device_write()?;
        write::observe_device_write(block_id, buf.len())?;
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
        namespace::observe_device_flush()?;
        Ok(())
    }
}

fn formatted_test_storage() -> (Arc<StdMutex<Vec<u8>>>, Arc<AtomicUsize>) {
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

    (storage, flushes)
}

fn test_filesystem(readonly_fallback: bool) -> (Ext4Filesystem, Arc<AtomicUsize>) {
    let (storage, flushes) = formatted_test_storage();

    if readonly_fallback {
        let mut storage = storage.lock().unwrap();
        let superblock_offset = usize::try_from(SUPERBLOCK_OFFSET).unwrap();
        let superblock_bytes = &mut storage[superblock_offset..superblock_offset + SUPERBLOCK_SIZE];
        let mut superblock = Ext4Superblock::from_disk_bytes(superblock_bytes);
        assert_eq!(superblock.s_magic, EXT4_SUPER_MAGIC);
        superblock.s_state |= Ext4Superblock::EXT4_ERROR_FS;
        superblock.update_checksum();
        superblock.to_disk_bytes(superblock_bytes);
    }

    mount_test_storage(storage, flushes, readonly_fallback)
}

fn mount_test_storage(
    storage: Arc<StdMutex<Vec<u8>>>,
    flushes: Arc<AtomicUsize>,
    readonly_fallback: bool,
) -> (Ext4Filesystem, Arc<AtomicUsize>) {
    let blocks = (TEST_DEVICE_BYTES / TEST_SECTOR_BYTES) as u64;

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
            inode_metadata: ext4.inode_metadata_reader(),
            inner: Mutex::new(Ext4State {
                ext4,
                lifetimes: InodeLifetimeTracker::default(),
                shutdown_attempted: false,
                dirty: false,
                staging: false,
            }),
            mmp_worker: MountWorker::disabled(),
            writeback: Writeback::new(false),
            inode_access: Mutex::new(BTreeMap::new()),
            namespace: Namespace::new(),
            admission: Admission::new(),
            cached_writes: Admission::new(),
            root_dir: Mutex::new(None),
            mount_lease: Mutex::new(Weak::new()),
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

#[test]
fn final_root_reference_releases_the_backing_device() {
    let (storage, flushes) = formatted_test_storage();
    let backing = Arc::downgrade(&storage);
    let device = SharedMemoryDevice {
        storage,
        read_only: false,
        flushes,
    };
    let filesystem = Ext4Filesystem::new(
        Box::new(device),
        BlockRegion::from_num_blocks((TEST_DEVICE_BYTES / TEST_SECTOR_BYTES) as u64),
    )
    .expect("mount the formatted device");
    let root = filesystem.root_dir();

    drop(filesystem);
    root.metadata()
        .expect("the root keeps its filesystem usable");
    assert!(backing.upgrade().is_some());

    drop(root);
    assert!(
        backing.upgrade().is_none(),
        "the final filesystem reference must release its backing device"
    );
}
