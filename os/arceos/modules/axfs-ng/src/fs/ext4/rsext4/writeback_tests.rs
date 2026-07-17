//! Ext4 buffered-write durability boundary regressions.

use alloc::{sync::Arc, vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_kspin::SpinNoPreempt;
use axfs_ng_vfs::{MetadataUpdate, Mountpoint, NodePermission, NodeType};

use super::{Ext4Disk, Ext4Filesystem};
use crate::{
    BlockDeviceMetadata, FsRuntime,
    block::{BlockDevice as FsBlockDevice, BlockRegion},
    file::{File, FileBackend},
    fs_core::FsContext,
    os::memory::{PAGE_SIZE, test_support::with_test_page_provider},
};

const TEST_DISK_BYTES: usize = 32 * 1024 * 1024;
const TEST_DISK_BLOCK_SIZE: usize = 512;

struct CountingMemoryDevice {
    bytes: SpinNoPreempt<alloc::vec::Vec<u8>>,
    flushes: AtomicUsize,
}

impl CountingMemoryDevice {
    fn new() -> Self {
        Self {
            bytes: SpinNoPreempt::new(vec![0; TEST_DISK_BYTES]),
            flushes: AtomicUsize::new(0),
        }
    }

    fn flushes(&self) -> usize {
        self.flushes.load(Ordering::Acquire)
    }

    fn reset_flushes(&self) {
        self.flushes.store(0, Ordering::Release);
    }
}

impl FsBlockDevice for CountingMemoryDevice {
    fn name(&self) -> &str {
        "ext4-writeback-test"
    }

    fn metadata(&self) -> BlockDeviceMetadata {
        BlockDeviceMetadata::new(
            (TEST_DISK_BYTES / TEST_DISK_BLOCK_SIZE) as u64,
            TEST_DISK_BLOCK_SIZE,
        )
        .unwrap()
    }

    fn read_blocks(&self, start_block: u64, buffer: &mut [u8]) -> ax_errno::AxResult {
        self.metadata()
            .validate_transfer(start_block, buffer.len())?;
        let start = start_block as usize * TEST_DISK_BLOCK_SIZE;
        buffer.copy_from_slice(&self.bytes.lock()[start..start + buffer.len()]);
        Ok(())
    }

    fn write_blocks(&self, start_block: u64, buffer: &[u8]) -> ax_errno::AxResult {
        self.metadata()
            .validate_transfer(start_block, buffer.len())?;
        let start = start_block as usize * TEST_DISK_BLOCK_SIZE;
        self.bytes.lock()[start..start + buffer.len()].copy_from_slice(buffer);
        Ok(())
    }

    fn flush(&self) -> ax_errno::AxResult {
        self.flushes.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[test]
fn explicit_sync_survives_a_fresh_mount() {
    with_test_page_provider(true, |_| {
        let (device, region) = formatted_device();
        let filesystem = Ext4Filesystem::new(device.clone(), region).unwrap();
        let mountpoint = Mountpoint::new_root(&filesystem);
        let runtime = FsRuntime::new_mounted();
        let context = FsContext::new_managed(
            mountpoint.root_location(),
            runtime.clone(),
            runtime.snapshot().generation,
        );
        let file = File::create(&context, "/buffered-write").unwrap();
        let cached = match file.backend().unwrap() {
            FileBackend::Cached(cached) => cached.clone(),
            _ => panic!("ordinary ext4 files must use the page cache"),
        };
        device.reset_flushes();

        let mut expected = alloc::vec::Vec::new();
        for page_index in 0..4 {
            let page = vec![page_index as u8 + 1; PAGE_SIZE];
            assert_eq!(
                cached
                    .write_at(&page[..], (page_index * PAGE_SIZE) as u64)
                    .unwrap(),
                PAGE_SIZE
            );
            expected.extend_from_slice(&page);
        }
        let appended = vec![0xa5; PAGE_SIZE];
        assert_eq!(
            cached.append(&appended[..]).unwrap(),
            (PAGE_SIZE, (5 * PAGE_SIZE) as u64)
        );
        expected.extend_from_slice(&appended);
        cached.set_len((6 * PAGE_SIZE) as u64).unwrap();
        expected.resize(6 * PAGE_SIZE, 0);

        assert_eq!(device.flushes(), 0);
        cached.sync(false).unwrap();
        assert!(device.flushes() > 0);

        let remounted = Ext4Filesystem::new(device, region).unwrap();
        let remounted_entry = remounted
            .root_dir()
            .as_dir()
            .unwrap()
            .lookup("buffered-write")
            .unwrap();
        let remounted_file = remounted_entry.as_file().unwrap();
        assert_eq!(remounted_file.len().unwrap(), expected.len() as u64);
        let mut persisted = vec![0; expected.len()];
        assert_eq!(
            remounted_file.read_at(&mut persisted, 0).unwrap(),
            persisted.len()
        );
        assert_eq!(persisted, expected);
    });
}

#[test]
fn open_unlinked_mutations_remain_buffered_until_explicit_sync() {
    let (device, region) = formatted_device();
    let filesystem = Ext4Filesystem::new(device.clone(), region).unwrap();
    let root = filesystem.root_dir();
    let entry = root
        .as_dir()
        .unwrap()
        .create(
            "open-unlinked",
            NodeType::RegularFile,
            NodePermission::from_bits_truncate(0o600),
            0,
            0,
        )
        .unwrap();
    let file = entry.as_file().unwrap();
    root.as_dir()
        .unwrap()
        .unlink("open-unlinked", false)
        .unwrap();
    device.reset_flushes();

    assert_eq!(file.write_at(b"first", 0).unwrap(), 5);
    assert_eq!(file.append(b"-second").unwrap(), (7, 12));
    file.set_len(4096).unwrap();

    assert_eq!(device.flushes(), 0);
    file.sync(false).unwrap();
    assert!(device.flushes() > 0);

    let mut contents = [0; 12];
    assert_eq!(file.read_at(&mut contents, 0).unwrap(), contents.len());
    assert_eq!(&contents, b"first-second");
}

#[test]
fn metadata_rename_remains_buffered_until_explicit_sync() {
    let (device, region) = formatted_device();
    let filesystem = Ext4Filesystem::new(device.clone(), region).unwrap();
    let root = filesystem.root_dir();
    let root_dir = root.as_dir().unwrap();
    let source_dir = root_dir
        .create(
            "rename-source",
            NodeType::Directory,
            NodePermission::from_bits_truncate(0o755),
            0,
            0,
        )
        .unwrap();
    let destination_dir = root_dir
        .create(
            "rename-destination",
            NodeType::Directory,
            NodePermission::from_bits_truncate(0o755),
            0,
            0,
        )
        .unwrap();
    let source_dir = source_dir.as_dir().unwrap();
    let destination_dir = destination_dir.as_dir().unwrap();
    source_dir
        .create(
            "source-file",
            NodeType::RegularFile,
            NodePermission::from_bits_truncate(0o600),
            0,
            0,
        )
        .unwrap();
    destination_dir
        .create(
            "destination-file",
            NodeType::RegularFile,
            NodePermission::from_bits_truncate(0o600),
            0,
            0,
        )
        .unwrap();
    device.reset_flushes();

    source_dir
        .rename("source-file", destination_dir, "destination-file")
        .unwrap();

    assert_eq!(device.flushes(), 0);
    assert!(source_dir.lookup("source-file").is_err());
    let renamed = destination_dir.lookup("destination-file").unwrap();
    renamed.as_file().unwrap().sync(false).unwrap();
    assert!(device.flushes() > 0);
}

#[test]
fn metadata_create_update_symlink_and_unlink_share_the_explicit_sync_boundary() {
    let (device, region) = formatted_device();
    let filesystem = Ext4Filesystem::new(device.clone(), region).unwrap();
    let root = filesystem.root_dir();
    let root_dir = root.as_dir().unwrap();
    let sync_anchor = root_dir
        .create(
            "sync-anchor",
            NodeType::RegularFile,
            NodePermission::from_bits_truncate(0o600),
            0,
            0,
        )
        .unwrap();
    device.reset_flushes();

    let created = root_dir
        .create(
            "metadata-victim",
            NodeType::RegularFile,
            NodePermission::from_bits_truncate(0o600),
            0,
            0,
        )
        .unwrap();
    assert_eq!(device.flushes(), 0);

    created
        .update_metadata(MetadataUpdate {
            mode: Some(NodePermission::from_bits_truncate(0o640)),
            ..MetadataUpdate::default()
        })
        .unwrap();
    assert_eq!(device.flushes(), 0);

    let symlink = root_dir
        .create(
            "metadata-symlink",
            NodeType::Symlink,
            NodePermission::from_bits_truncate(0o777),
            0,
            0,
        )
        .unwrap();
    symlink
        .as_file()
        .unwrap()
        .set_symlink("metadata-victim")
        .unwrap();
    assert_eq!(device.flushes(), 0);

    root_dir.unlink("metadata-victim", false).unwrap();
    assert_eq!(device.flushes(), 0);

    sync_anchor.sync(false).unwrap();
    assert!(device.flushes() > 0);
}

fn formatted_device() -> (Arc<CountingMemoryDevice>, BlockRegion) {
    let device = Arc::new(CountingMemoryDevice::new());
    let region = BlockRegion::from_num_blocks(device.metadata().num_blocks());
    let mut formatter =
        rsext4::Jbd2Dev::initial_jbd2dev(0, Ext4Disk::new(device.clone(), region).unwrap(), true);
    rsext4::mkfs(&mut formatter).unwrap();
    formatter.cantflush().unwrap();
    drop(formatter);
    (device, region)
}
