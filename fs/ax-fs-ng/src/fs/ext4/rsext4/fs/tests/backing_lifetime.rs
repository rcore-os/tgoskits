//! Backing-device lifetime across final mount retirement.
//!
//! The loop path hands its open lease to `new_from_file`, which stores it in
//! `FileImageDevice`. A retired cached inode may keep the filesystem alive
//! after the last mountpoint is gone, so the final mount lease must quiesce
//! and release the backing device instead of waiting for the filesystem drop.

use axfs_ng_vfs::{DirEntry, Filesystem, Location, Mountpoint, NodePermission, NodeType, VfsError};

use super::*;
use crate::{
    file::{CachedFile, FileBackend},
    os::memory::test_support::with_test_page_provider,
    vfs::new_filesystem_from_file,
};

/// The loop image lives in a real ext4 file. Counting this filesystem's block
/// device proves that operations on the retired image never reach the backing
/// store after its lease is released.
/// The backing store must also have room for the full image plus ext4 metadata.
const BACKING_DEVICE_BYTES: usize = 2 * TEST_DEVICE_BYTES;
const IMAGE_WRITE_CHUNK: usize = 8 * 1024 * 1024;

#[derive(Clone, Default)]
struct DeviceAccess {
    reads: Arc<AtomicUsize>,
    writes: Arc<AtomicUsize>,
    flushes: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct CountingImageDevice {
    storage: Arc<StdMutex<Vec<u8>>>,
    access: DeviceAccess,
}

impl FsBlockDevice for CountingImageDevice {
    fn fork_io(&self) -> BlockResult<Box<dyn FsBlockDevice>> {
        Ok(Box::new(self.clone()))
    }

    fn name(&self) -> &str {
        "ext4-backing-lifetime-test"
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
        false
    }

    fn supports_flush(&self) -> bool {
        true
    }

    fn supports_fua(&self) -> bool {
        false
    }

    fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> BlockResult {
        self.access.reads.fetch_add(1, Ordering::SeqCst);
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
        self.access.writes.fetch_add(1, Ordering::SeqCst);
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
        self.access.flushes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Counts the one real release of the file-backed device guard.
struct BackingGuard(Arc<AtomicUsize>);

impl Drop for BackingGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct LoopImageFixture {
    filesystem: Filesystem,
    mount: Arc<Mountpoint>,
    root: Location,
    held: Location,
    cached: CachedFile,
    /// A real closed file/cache backing owner. Dropping every mountpoint and
    /// the cached handle must not drop this entry.
    retained: DirEntry,
    guard_drops: Arc<AtomicUsize>,
    access: DeviceAccess,
}

impl LoopImageFixture {
    fn mount() -> Self {
        let image_storage = format_storage(TEST_DEVICE_BYTES);
        let backing_storage = format_storage(BACKING_DEVICE_BYTES);
        let access = DeviceAccess::default();
        let backing = mount_counting_backing(backing_storage, access.clone());
        let backing_mount = Mountpoint::new_root(&backing);
        let backing_root = backing_mount.root_location();
        let image = backing_root
            .create(
                "loop.img",
                NodeType::RegularFile,
                NodePermission::default(),
                0,
                0,
            )
            .expect("create loop image");

        // The image and the filesystem that stores it use independent devices,
        // so mounting the inner image cannot corrupt its source bytes.
        let image_bytes = image_storage.lock().unwrap().clone();
        for start in (0..image_bytes.len()).step_by(IMAGE_WRITE_CHUNK) {
            let end = (start + IMAGE_WRITE_CHUNK).min(image_bytes.len());
            image
                .entry()
                .as_file()
                .expect("regular image file")
                .write_at(&image_bytes[start..end], start as u64)
                .expect("write loop image");
        }
        image.sync(false).expect("sync loop image");
        drop(backing_root);
        drop(backing_mount);
        drop(backing);

        let guard_drops = Arc::new(AtomicUsize::new(0));
        let filesystem = new_filesystem_from_file(
            FileBackend::new_direct(image),
            false,
            BackingGuard(Arc::clone(&guard_drops)),
        )
        .expect("mount ext4 from the file image");

        let mount = Mountpoint::new_root(&filesystem);
        let root = mount.root_location();
        let held = root
            .create(
                "held",
                NodeType::RegularFile,
                NodePermission::default(),
                0,
                0,
            )
            .expect("create retained file");
        let cached = CachedFile::get_or_create(held.clone()).expect("cache retained file");
        cached
            .write_at(b"cached".as_slice(), 0)
            .expect("populate retained file");
        cached.sync(false).expect("write back retained file");

        // This is the closed inode/cache owner that survives mount retirement.
        let retained = held.entry().clone();
        let mut bytes = [0; 6];
        assert_eq!(
            retained
                .as_file()
                .expect("regular retained file")
                .read_at(&mut bytes, 0),
            Ok(6)
        );
        assert_eq!(&bytes, b"cached");

        Self {
            filesystem,
            mount,
            root,
            held,
            cached,
            retained,
            guard_drops,
            access,
        }
    }
}

fn format_storage(bytes: usize) -> Arc<StdMutex<Vec<u8>>> {
    let storage = Arc::new(StdMutex::new(alloc::vec![0; bytes]));
    let blocks = (bytes / TEST_SECTOR_BYTES) as u64;
    let device = SharedMemoryDevice {
        storage: Arc::clone(&storage),
        read_only: false,
        flushes: Arc::new(AtomicUsize::new(0)),
    };
    let disk =
        Ext4Disk::new(Box::new(device), BlockRegion::from_num_blocks(blocks)).expect("geometry");
    rsext4::format(disk, Ext4Clock, MkfsOptions::default()).expect("format backing image");
    storage
}

/// Mirrors `mount_test_storage` so the backing store never starts a runtime
/// writeback worker that could access the device after the guard is released.
fn mount_counting_backing(storage: Arc<StdMutex<Vec<u8>>>, access: DeviceAccess) -> Filesystem {
    let blocks = (BACKING_DEVICE_BYTES / TEST_SECTOR_BYTES) as u64;
    let disk = Ext4Disk::new(
        Box::new(CountingImageDevice { storage, access }),
        BlockRegion::from_num_blocks(blocks),
    )
    .expect("valid backing-device geometry");
    let services = MountServices::new(Ext4Clock, Ext4Entropy, Ext4Observer)
        .with_mmp(Ext4Delay, MmpIdentity::default());
    let ext4 = rsext4::Ext4::mount_with_readonly_fallback(disk, services)
        .expect("mount backing image store");
    assert!(!ext4.options().readonly);
    let filesystem = Arc::new_cyclic(|self_ref| Ext4Filesystem {
        self_ref: self_ref.clone(),
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
        mount_lease: Mutex::new(MountLeaseState::default()),
        shutdown_state: Mutex::new(ShutdownState::default()),
        #[cfg(feature = "vfs")]
        backing_lease: None,
    });
    Filesystem::new(filesystem)
}

#[test]
fn final_mount_retirement_releases_the_backing_guard_before_cached_inode_drops() {
    with_test_page_provider(true, |_| {
        let LoopImageFixture {
            filesystem,
            mount,
            root,
            held,
            cached,
            retained,
            guard_drops,
            access,
        } = LoopImageFixture::mount();

        drop(cached);
        drop(held);
        drop(root);
        drop(mount);

        // Stable red signal for the old implementation: only the filesystem
        // drop released the image-device lease, and `retained` still holds it.
        assert_eq!(
            guard_drops.load(Ordering::SeqCst),
            1,
            "the final mount lease must release the file backing guard"
        );

        let reads = access.reads.load(Ordering::SeqCst);
        let writes = access.writes.load(Ordering::SeqCst);
        let flushes = access.flushes.load(Ordering::SeqCst);

        let file = retained.as_file().expect("regular retained file");
        let mut late = [0; 6];
        assert_eq!(
            file.read_at(&mut late, 0),
            Err(VfsError::ResourceBusy),
            "a retired filesystem must reject reads"
        );
        assert_eq!(
            file.write_at(b"late!!".as_slice(), 0),
            Err(VfsError::ResourceBusy),
            "a retired filesystem must reject writes"
        );
        assert_eq!(
            retained.sync(false),
            Err(VfsError::ResourceBusy),
            "a retired filesystem must reject flushes"
        );
        // Retaining the original Filesystem handle must not make the retired
        // instance mountable again. A fresh Mountpoint receives no lifetime
        // lease, and every path into the filesystem is still rejected by the
        // closed admission gate before it can reach the released device.
        let late_mount = Mountpoint::new_root(&filesystem);
        let late_root = late_mount.root_location();
        assert!(
            late_root.filesystem().mount_lease().is_none(),
            "a retired filesystem must not publish another lifetime lease"
        );
        assert_eq!(
            late_root.lookup_no_follow("held").unwrap_err(),
            VfsError::ResourceBusy,
            "a retired filesystem must reject a new location"
        );
        assert_eq!(
            late_root
                .create(
                    "late",
                    NodeType::RegularFile,
                    NodePermission::default(),
                    0,
                    0,
                )
                .unwrap_err(),
            VfsError::ResourceBusy,
            "a retired filesystem must reject a new file"
        );
        assert_eq!(
            access.reads.load(Ordering::SeqCst),
            reads,
            "a retired filesystem read the released backing device"
        );
        assert_eq!(
            access.writes.load(Ordering::SeqCst),
            writes,
            "a retired filesystem wrote the released backing device"
        );
        assert_eq!(
            access.flushes.load(Ordering::SeqCst),
            flushes,
            "a retired filesystem flushed the released backing device"
        );
        drop(late_root);
        drop(late_mount);
        drop(filesystem);
        assert_eq!(
            guard_drops.load(Ordering::SeqCst),
            1,
            "a retained filesystem handle must not release the guard again"
        );

        drop(retained);
        assert_eq!(
            guard_drops.load(Ordering::SeqCst),
            1,
            "the RAII fallback must not release the guard twice"
        );
    });
}

#[test]
fn backing_guard_stays_until_the_last_mount_owner_leaves() {
    with_test_page_provider(true, |_| {
        let fixture = LoopImageFixture::mount();
        // A second mountpoint shares the same filesystem lease, like a bind
        // alias or namespace copy.
        let alias = Mountpoint::new_root(&fixture.filesystem);
        let alias_root = alias.root_location();
        let guard_drops = Arc::clone(&fixture.guard_drops);
        let LoopImageFixture {
            filesystem,
            mount,
            root,
            held,
            cached,
            retained,
            ..
        } = fixture;

        drop(root);
        drop(mount);
        assert_eq!(
            guard_drops.load(Ordering::SeqCst),
            0,
            "an independent mountpoint still holds the lease"
        );

        drop(cached);
        drop(held);
        assert_eq!(
            guard_drops.load(Ordering::SeqCst),
            0,
            "a live location still holds the lease"
        );

        drop(alias_root);
        drop(alias);
        drop(filesystem);
        assert_eq!(
            guard_drops.load(Ordering::SeqCst),
            1,
            "the last mount lease must release the guard while the inode owner remains"
        );

        drop(retained);
        assert_eq!(
            guard_drops.load(Ordering::SeqCst),
            1,
            "dropping the remaining owner must not release the guard again"
        );
    });
}
