//! End-to-end integration tests for the core filesystem flows.
//!
//! These tests exercise mkfs, mount, directory creation, file IO, and the
//! public API surface together so regressions show up as user-visible failures.

use std::cell::Cell;

use rsext4::{
    error::{Ext4Error, Ext4Result},
    extents_tree::{ExtentNode, ExtentTree},
    *,
};

/// Simple in-memory block device used by the integration tests.
struct TestBlockDevice {
    data: Vec<u8>,
    block_size: u32,
    now: Cell<i64>,
}

impl TestBlockDevice {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            block_size: rsext4::BLOCK_SIZE as u32, // Match the ext4 block size used by the crate.
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockIo for TestBlockDevice {
    fn read(&mut self, buffer: &mut [u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: rsext4::SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start + buffer.len();
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                (self.data.len() / self.block_size as usize) as u64,
            ));
        }
        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn geometry(&self) -> rsext4::DeviceGeometry {
        rsext4::DeviceGeometry::new(self.block_size, {
            (self.data.len() / self.block_size as usize) as u64
        })
    }

    fn capabilities(&self) -> rsext4::DeviceCapabilities {
        rsext4::DeviceCapabilities {
            read_only: { false },

            flush: true,

            ..rsext4::DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> rsext4::Ext4Result<()> {
        Ok(())
    }
}

impl rsext4::Clock for TestBlockDevice {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

struct IoOnlyDevice {
    data: Vec<u8>,
    block_size: u32,
}

impl From<TestBlockDevice> for IoOnlyDevice {
    fn from(device: TestBlockDevice) -> Self {
        Self {
            data: device.data,
            block_size: device.block_size,
        }
    }
}

impl BlockIo for IoOnlyDevice {
    fn read(&mut self, buffer: &mut [u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(Ext4Error::overflow)?;
        let source = self.data.get(start..end).ok_or_else(|| {
            Ext4Error::block_out_of_range(
                sector.to_u32().unwrap_or(u32::MAX),
                self.geometry().block_count,
            )
        })?;
        buffer.copy_from_slice(source);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, _count: u32) -> Ext4Result<()> {
        let start = sector.as_usize()? * self.block_size as usize;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(Ext4Error::overflow)?;
        let block_count = self.geometry().block_count;
        let destination = self.data.get_mut(start..end).ok_or_else(|| {
            Ext4Error::block_out_of_range(sector.to_u32().unwrap_or(u32::MAX), block_count)
        })?;
        destination.copy_from_slice(buffer);
        Ok(())
    }

    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(
            self.block_size,
            (self.data.len() / self.block_size as usize) as u64,
        )
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            flush: true,
            ..DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> Ext4Result<()> {
        Ok(())
    }
}

struct SeparateClock(Cell<i64>);

impl Clock for SeparateClock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        let seconds = self.0.get();
        self.0.set(seconds + 1);
        Ok(Ext4Timestamp::new(seconds, 0))
    }
}

struct UnavailableCapabilities;

impl EntropySource for UnavailableCapabilities {
    fn fill_bytes(&mut self, _output: &mut [u8]) -> Ext4Result<()> {
        Err(Ext4Error::unsupported_capability("entropy"))
    }
}

impl CryptoProvider for UnavailableCapabilities {
    fn crypt(
        &mut self,
        _operation: CryptoOperation,
        _algorithm: EncryptionAlgorithm,
        _key: &[u8],
        _nonce: &[u8],
        _input: &[u8],
        _output: &mut [u8],
    ) -> Ext4Result<()> {
        Err(Ext4Error::unsupported_capability("crypto"))
    }

    fn digest(
        &mut self,
        _algorithm: DigestAlgorithm,
        _input: &[u8],
        _output: &mut [u8],
    ) -> Ext4Result<usize> {
        Err(Ext4Error::unsupported_capability("crypto"))
    }
}

impl KeyProvider for UnavailableCapabilities {
    fn read_key(
        &mut self,
        _descriptor: KeyDescriptor<'_>,
        _output: &mut [u8],
    ) -> Ext4Result<usize> {
        Err(Ext4Error::unsupported_capability("keys"))
    }
}

#[derive(Default)]
struct RecordingObserver {
    events: Vec<Event>,
}

impl Observer for RecordingObserver {
    fn event(&mut self, event: Event) {
        self.events.push(event);
    }
}

#[test]
fn owned_mount_injects_clock_separately_from_block_io() {
    let device = TestBlockDevice::new(100 * 1024 * 1024);
    let mut builder = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut builder).expect("mkfs failed");
    let device = IoOnlyDevice::from(builder.into_inner());
    let services = MountServices::new(
        SeparateClock(Cell::new(1_800_000_000)),
        UnavailableCapabilities,
        UnavailableCapabilities,
        UnavailableCapabilities,
        RecordingObserver::default(),
    );

    let mut filesystem =
        Ext4::mount(device, services, MountOptions::read_write()).expect("owned mount failed");
    let root = filesystem
        .inode(filesystem.root_inode())
        .expect("root inode inspection failed");

    assert_eq!(root.number.raw(), 2);
    assert_ne!(root.mode & rsext4::disknode::Ext4Inode::S_IFDIR, 0);
    let lost_found = filesystem
        .lookup_child(
            root.number,
            FileName::new(b"lost+found").expect("valid raw name"),
        )
        .expect("child lookup failed")
        .expect("lost+found missing");
    assert_ne!(lost_found.mode & rsext4::disknode::Ext4Inode::S_IFDIR, 0);

    let entries = filesystem
        .read_directory(root.number, 0, 16)
        .expect("root readdir failed");
    assert!(entries.iter().any(|entry| entry.name == b"."));
    assert!(entries.iter().any(|entry| entry.name == b".."));
    assert!(entries.iter().any(|entry| entry.name == b"lost+found"));
    assert!(
        entries
            .windows(2)
            .all(|pair| pair[0].next_offset < pair[1].next_offset)
    );

    let raw_non_utf8 = FileName::new(&[0xff]).expect("ext4 names are raw bytes");
    assert!(
        filesystem
            .lookup_child(root.number, raw_non_utf8)
            .expect("raw lookup failed")
            .is_none()
    );
    assert!(FileName::new(b"").is_err());
    assert!(FileName::new(b"a/b").is_err());
    assert!(FileName::new(b"a\0b").is_err());
    filesystem.unmount().expect("owned unmount failed");
}

#[test]
fn observer_receives_typed_mount_and_unmount_transitions() {
    let device = TestBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut observer = RecordingObserver::default();
    let mut fs =
        mount_with_options_and_observer(&mut jbd2_dev, MountOptions::read_write(), &mut observer)
            .expect("observed mount failed");
    fs.umount_with_observer(&mut jbd2_dev, &mut observer)
        .expect("observed unmount failed");

    assert_eq!(
        observer.events.first(),
        Some(&Event::Mount(rsext4::runtime::MountEvent::Started))
    );
    assert!(
        observer
            .events
            .contains(&Event::Mount(rsext4::runtime::MountEvent::Succeeded))
    );
    assert!(
        observer
            .events
            .contains(&Event::Mount(rsext4::runtime::MountEvent::UnmountStarted))
    );
    assert_eq!(
        observer.events.last(),
        Some(&Event::Mount(rsext4::runtime::MountEvent::Unmounted))
    );
}

#[test]
fn test_basic_mount_mkfs() {
    let device = TestBlockDevice::new(100 * 1024 * 1024); // 100MB
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

    // Test idea: create a fresh filesystem, perform one full create/read cycle,
    // and then unmount cleanly to prove the basic happy path works.
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut fs = mount(&mut jbd2_dev).expect("mount failed");

    mkdir(&mut jbd2_dev, &mut fs, "/test").expect("mkdir failed");

    let data = b"Hello, world!";
    mkfile(&mut jbd2_dev, &mut fs, "/test/hello.txt", Some(data), None).expect("mkfile failed");

    let read_data = read_file(&mut jbd2_dev, &mut fs, "/test/hello.txt").expect("read_file failed");
    assert_eq!(read_data, data.to_vec());

    umount(fs, &mut jbd2_dev).expect("umount failed");
}

#[test]
fn overlong_directory_name_is_rejected_without_truncation() {
    let device = TestBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let mut fs = mount(&mut jbd2_dev).expect("mount failed");
    let overlong = "a".repeat(256);
    let path = format!("/{overlong}");

    let error = mkfile(&mut jbd2_dev, &mut fs, &path, None, None)
        .expect_err("an overlong name must not be truncated");
    assert_eq!(error.kind(), Ext4ErrorKind::InvalidInput);
    assert!(
        rsext4::loopfile::get_file_inode(&mut fs, &mut jbd2_dev, &format!("/{}", "a".repeat(255)),)
            .expect("lookup failed")
            .is_none()
    );
}

#[test]
fn test_file_operations() {
    let device = TestBlockDevice::new(100 * 1024 * 1024); // 100MB
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let mut fs = mount(&mut jbd2_dev).expect("mount failed");

    // Test idea: mix high-level file helpers with the public open/read/write API
    // and verify that both views observe the same file contents.
    mkdir(&mut jbd2_dev, &mut fs, "/filetest").expect("mkdir failed");

    mkfile(&mut jbd2_dev, &mut fs, "/filetest/empty.txt", None, None).expect("mkfile failed");

    write_file(
        &mut jbd2_dev,
        &mut fs,
        "/filetest/empty.txt",
        0,
        b"First line",
    )
    .expect("write_file failed");

    // Append through the path-based helper and verify the concatenated content.
    let file_len = read_file(&mut jbd2_dev, &mut fs, "/filetest/empty.txt")
        .expect("read_file failed")
        .len();
    write_file(
        &mut jbd2_dev,
        &mut fs,
        "/filetest/empty.txt",
        file_len as u64,
        b"\nSecond line",
    )
    .expect("write_file failed");

    let data = read_file(&mut jbd2_dev, &mut fs, "/filetest/empty.txt").expect("read_file failed");
    assert_eq!(data, b"First line\nSecond line".to_vec());

    // Then switch to the descriptor-style API and validate that open/write/read
    // observe the same backing state.
    let mut file = open(&mut jbd2_dev, &mut fs, "/filetest/api.txt", true).expect("open failed");

    write_at(&mut jbd2_dev, &mut fs, &mut file, b"API test").expect("write_at failed");
    lseek(&mut file, 0).expect("lseek failed");

    let bytes_read = read_at(&mut jbd2_dev, &mut fs, &mut file, 8).expect("read_at failed");
    assert_eq!(bytes_read, b"API test");

    umount(fs, &mut jbd2_dev).expect("umount failed");
}

#[test]
fn large_sequential_write_uses_one_contiguous_extent() {
    let device = TestBlockDevice::new(128 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let mut fs = mount(&mut jbd2_dev).expect("mount failed");

    mkdir(&mut jbd2_dev, &mut fs, "/large").expect("mkdir failed");
    mkfile(&mut jbd2_dev, &mut fs, "/large/run.bin", None, None).expect("mkfile failed");

    let blocks = 20 * 1024 * 1024 / BLOCK_SIZE;
    let mut data = vec![0u8; blocks * BLOCK_SIZE];
    for (idx, byte) in data.iter_mut().enumerate() {
        *byte = (idx as u8).wrapping_mul(31).wrapping_add(7);
    }

    write_file(&mut jbd2_dev, &mut fs, "/large/run.bin", 0, &data).expect("large write failed");

    let read_back = read_file(&mut jbd2_dev, &mut fs, "/large/run.bin").expect("read failed");
    assert_eq!(read_back.len(), data.len());
    assert_eq!(&read_back[..BLOCK_SIZE], &data[..BLOCK_SIZE]);
    assert_eq!(
        &read_back[data.len() - BLOCK_SIZE..],
        &data[data.len() - BLOCK_SIZE..]
    );

    let mut inode = find_file(&mut fs, &mut jbd2_dev, "/large/run.bin").expect("find failed");
    let tree = ExtentTree::with_filesystem(&mut inode, &fs, fs.root_inode);
    match tree.load_root_from_inode().expect("extent root") {
        ExtentNode::Leaf { entries, .. } => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].ee_block, 0);
            assert_eq!(entries[0].len() as usize, blocks);
            assert!(entries[0].is_initialized());
        }
        ExtentNode::Index { .. } => panic!("large sequential write should stay one leaf extent"),
    }

    umount(fs, &mut jbd2_dev).expect("umount failed");
}
