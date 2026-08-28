//! End-to-end integration tests for the core filesystem flows.
//!
//! These tests exercise mkfs, mount, directory creation, file IO, and the
//! public API surface together so regressions show up as user-visible failures.

use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

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

#[derive(Default)]
struct SyncIoCounters {
    reads: AtomicUsize,
    writes: AtomicUsize,
    written_sectors: AtomicUsize,
    primary_superblock_writes: AtomicUsize,
    primary_gdt_writes: AtomicUsize,
    flushes: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SyncIoSnapshot {
    reads: usize,
    writes: usize,
    written_sectors: usize,
    primary_superblock_writes: usize,
    primary_gdt_writes: usize,
    flushes: usize,
}

impl SyncIoCounters {
    fn reset(&self) {
        self.reads.store(0, Ordering::SeqCst);
        self.writes.store(0, Ordering::SeqCst);
        self.written_sectors.store(0, Ordering::SeqCst);
        self.primary_superblock_writes.store(0, Ordering::SeqCst);
        self.primary_gdt_writes.store(0, Ordering::SeqCst);
        self.flushes.store(0, Ordering::SeqCst);
    }

    fn snapshot(&self) -> SyncIoSnapshot {
        SyncIoSnapshot {
            reads: self.reads.load(Ordering::SeqCst),
            writes: self.writes.load(Ordering::SeqCst),
            written_sectors: self.written_sectors.load(Ordering::SeqCst),
            primary_superblock_writes: self.primary_superblock_writes.load(Ordering::SeqCst),
            primary_gdt_writes: self.primary_gdt_writes.load(Ordering::SeqCst),
            flushes: self.flushes.load(Ordering::SeqCst),
        }
    }
}

struct CountingIoDevice {
    inner: IoOnlyDevice,
    counters: Arc<SyncIoCounters>,
}

impl CountingIoDevice {
    fn new(size: usize, counters: Arc<SyncIoCounters>) -> Self {
        Self {
            inner: IoOnlyDevice::from(TestBlockDevice::new(size)),
            counters,
        }
    }
}

impl BlockIo for CountingIoDevice {
    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        self.counters.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read(buffer, sector, count)
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        self.counters.writes.fetch_add(1, Ordering::SeqCst);
        self.counters
            .written_sectors
            .fetch_add(count as usize, Ordering::SeqCst);
        // The test device and default mkfs both use 4 KiB blocks, so the
        // primary superblock shares sector 0 and the GDT starts at sector 1.
        if sector.raw() == 0 {
            self.counters
                .primary_superblock_writes
                .fetch_add(1, Ordering::SeqCst);
        }
        if sector.raw() == 1 {
            self.counters
                .primary_gdt_writes
                .fetch_add(1, Ordering::SeqCst);
        }
        self.inner.write(buffer, sector, count)
    }

    fn geometry(&self) -> DeviceGeometry {
        self.inner.geometry()
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.inner.capabilities()
    }

    fn flush(&mut self) -> Ext4Result<()> {
        self.counters.flushes.fetch_add(1, Ordering::SeqCst);
        self.inner.flush()
    }
}

struct IoOnlyDevice {
    data: Vec<u8>,
    block_size: u32,
    read_only: bool,
    fail_flush: Option<Arc<AtomicBool>>,
}

impl From<TestBlockDevice> for IoOnlyDevice {
    fn from(device: TestBlockDevice) -> Self {
        Self {
            data: device.data,
            block_size: device.block_size,
            read_only: false,
            fail_flush: None,
        }
    }
}

impl IoOnlyDevice {
    fn into_read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    fn with_flush_failure(mut self, fail_flush: Arc<AtomicBool>) -> Self {
        self.fail_flush = Some(fail_flush);
        self
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
        if self.read_only {
            return Err(Ext4Error::read_only());
        }
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
            read_only: self.read_only,
            flush: true,
            ..DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> Ext4Result<()> {
        if self
            .fail_flush
            .as_ref()
            .is_some_and(|failure| failure.swap(false, Ordering::SeqCst))
        {
            Err(Ext4Error::io())
        } else {
            Ok(())
        }
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

#[derive(Default)]
struct RecordingObserver {
    events: Vec<Event>,
}

impl Observer for RecordingObserver {
    fn event(&mut self, event: Event) {
        self.events.push(event);
    }
}

type TestOwnedFilesystem =
    Ext4<IoOnlyDevice, MountedServices<UnavailableCapabilities, RecordingObserver>>;

fn owned_test_filesystem() -> TestOwnedFilesystem {
    let device = IoOnlyDevice::from(TestBlockDevice::new(100 * 1024 * 1024));
    let device = format(
        device,
        SeparateClock(Cell::new(1_700_000_000)),
        MkfsOptions::default(),
    )
    .expect("mkfs failed");
    let services = MountServices::new(
        SeparateClock(Cell::new(1_800_000_000)),
        UnavailableCapabilities,
        RecordingObserver::default(),
    );
    Ext4::mount(device, services, MountOptions::read_write()).expect("owned mount failed")
}

#[test]
fn owned_directory_change_attribute_tracks_persistent_name_mutations() {
    let mut filesystem = owned_test_filesystem();
    let root = filesystem.root_inode();
    let context = MutationContext::new(1000, 1001, 0, 0o022);
    let before = filesystem.inode(root).expect("inspect root before create");

    let file = filesystem
        .create_regular_file(
            context,
            root,
            FileName::new(b"versioned-name").expect("valid raw name"),
            FilePermissions::new(0o644).expect("valid permissions"),
        )
        .expect("create file");
    let after_create = filesystem.inode(root).expect("inspect root after create");
    assert!(after_create.change_attribute > before.change_attribute);

    let _ = filesystem
        .unlink(
            root,
            FileName::new(b"versioned-name").expect("valid raw name"),
        )
        .expect("unlink file");
    let after_unlink = filesystem.inode(root).expect("inspect root after unlink");
    assert!(after_unlink.change_attribute > after_create.change_attribute);

    filesystem
        .reap_unlinked_inode(file.number)
        .expect("reap unlinked inode");
}

fn owned_test_filesystem_with_flush_failure() -> (TestOwnedFilesystem, Arc<AtomicBool>) {
    let fail_flush = Arc::new(AtomicBool::new(false));
    let device = IoOnlyDevice::from(TestBlockDevice::new(100 * 1024 * 1024))
        .with_flush_failure(fail_flush.clone());
    let device = format(
        device,
        SeparateClock(Cell::new(1_700_000_000)),
        MkfsOptions::default(),
    )
    .expect("mkfs failed");
    let services = MountServices::new(
        SeparateClock(Cell::new(1_800_000_000)),
        UnavailableCapabilities,
        RecordingObserver::default(),
    );
    let filesystem =
        Ext4::mount(device, services, MountOptions::read_write()).expect("owned mount failed");
    (filesystem, fail_flush)
}

#[test]
fn clean_sync_does_not_rewrite_clean_metadata() {
    let counters = Arc::new(SyncIoCounters::default());
    let device = CountingIoDevice::new(100 * 1024 * 1024, counters.clone());
    let device = format(
        device,
        SeparateClock(Cell::new(1_700_000_000)),
        MkfsOptions::default(),
    )
    .expect("mkfs failed");
    let services = MountServices::new(
        SeparateClock(Cell::new(1_800_000_000)),
        UnavailableCapabilities,
        RecordingObserver::default(),
    );
    let mut filesystem =
        Ext4::mount(device, services, MountOptions::read_write()).expect("owned mount failed");

    counters.reset();
    filesystem.sync().expect("clean sync failed");

    assert_eq!(
        counters.primary_superblock_writes.load(Ordering::SeqCst),
        0,
        "a clean sync must not serialize an unchanged superblock"
    );
    assert_eq!(
        counters.primary_gdt_writes.load(Ordering::SeqCst),
        0,
        "a clean sync must not serialize unchanged group descriptors"
    );
    assert!(
        counters.flushes.load(Ordering::SeqCst) > 0,
        "a clean sync must still preserve the device durability boundary"
    );
}

#[test]
fn sync_cycle_keeps_dirty_clean_and_unmount_io_boundaries_distinct() {
    let counters = Arc::new(SyncIoCounters::default());
    let device = CountingIoDevice::new(100 * 1024 * 1024, counters.clone());
    let device = format(
        device,
        SeparateClock(Cell::new(1_700_000_000)),
        MkfsOptions::default(),
    )
    .expect("mkfs failed");
    let services = MountServices::new(
        SeparateClock(Cell::new(1_800_000_000)),
        UnavailableCapabilities,
        RecordingObserver::default(),
    );
    let mut filesystem =
        Ext4::mount(device, services, MountOptions::read_write()).expect("owned mount failed");
    let context = MutationContext::new(0, 0, 0, 0);
    let file = filesystem
        .create_regular_file(
            context,
            filesystem.root_inode(),
            FileName::new(b"sync-cycle").expect("valid name"),
            FilePermissions::new(0o644).expect("valid permissions"),
        )
        .expect("create file");
    filesystem
        .write_inode(file.number, 0, &[0x5a; BLOCK_SIZE])
        .expect("write file");

    counters.reset();
    filesystem.sync().expect("dirty sync");
    let dirty_sync = counters.snapshot();

    counters.reset();
    filesystem.sync().expect("clean sync");
    let clean_sync = counters.snapshot();

    counters.reset();
    filesystem.unmount().expect("clean unmount");
    let unmount = counters.snapshot();

    assert!(dirty_sync.writes > 0);
    assert_eq!(
        dirty_sync.primary_superblock_writes, 0,
        "ordinary sync must leave committed metadata in the checkpoint owner"
    );
    assert_eq!(dirty_sync.primary_gdt_writes, 0);
    assert_eq!(dirty_sync.flushes, 2);
    assert_eq!(clean_sync.writes, 0);
    assert_eq!(clean_sync.flushes, 1);
    assert!(unmount.writes > 0);
    assert_eq!(unmount.primary_superblock_writes, 1);
    assert_eq!(unmount.primary_gdt_writes, 1);
    assert_eq!(unmount.flushes, 4);
}

#[test]
fn owned_mount_injects_clock_separately_from_block_io() {
    let device = IoOnlyDevice::from(TestBlockDevice::new(100 * 1024 * 1024));
    let device = format(
        device,
        SeparateClock(Cell::new(1_700_000_000)),
        MkfsOptions::default(),
    )
    .expect("mkfs failed");
    let services = MountServices::new(
        SeparateClock(Cell::new(1_800_000_000)),
        UnavailableCapabilities,
        RecordingObserver::default(),
    );

    let mut filesystem =
        Ext4::mount(device, services, MountOptions::read_write()).expect("owned mount failed");
    let root = filesystem
        .inode(filesystem.root_inode())
        .expect("root inode inspection failed");

    assert_eq!(root.number.raw(), 2);
    assert_eq!(root.file_type(), DirectoryEntryType::Directory);
    assert!(root.is_directory());
    let lost_found = filesystem
        .lookup_child(
            root.number,
            FileName::new(b"lost+found").expect("valid raw name"),
        )
        .expect("child lookup failed")
        .expect("lost+found missing");
    assert_eq!(lost_found.file_type(), DirectoryEntryType::Directory);

    let entries = filesystem
        .read_directory(root.number, DirectoryCursor::Start, 16)
        .expect("root readdir failed");
    assert!(entries.iter().any(|entry| entry.name == b"."));
    assert!(entries.iter().any(|entry| entry.name == b".."));
    assert!(entries.iter().any(|entry| entry.name == b"lost+found"));
    assert!(
        entries
            .windows(2)
            .all(|pair| match (pair[0].next_cursor, pair[1].next_cursor) {
                (
                    DirectoryCursor::Linear { offset: first },
                    DirectoryCursor::Linear { offset: second },
                ) => first < second,
                _ => false,
            })
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

    let context = MutationContext::new(1000, 1001, 7, 0o027);
    let raw_directory_name = FileName::new(&[b'd', 0xff]).expect("valid raw directory name");
    let raw_directory = filesystem
        .create_directory(
            context,
            root.number,
            raw_directory_name,
            FilePermissions::new(0o777).expect("valid directory permissions"),
        )
        .expect("raw directory create failed");
    assert_eq!(raw_directory.mode & 0o7777, 0o750);
    assert_eq!(raw_directory.uid, 1000);
    assert_eq!(raw_directory.gid, 1001);

    let raw_file_name = FileName::new(&[b'f', 0xfe]).expect("valid raw file name");
    let raw_file = filesystem
        .create_regular_file(
            context,
            raw_directory.number,
            raw_file_name,
            FilePermissions::new(0o666).expect("valid file permissions"),
        )
        .expect("raw file create failed");
    assert_eq!(raw_file.mode & 0o7777, 0o640);
    assert_eq!(raw_file.uid, 1000);
    assert_eq!(raw_file.gid, 1001);

    let raw_link_name = FileName::new(&[b'l', 0xfd]).expect("valid raw link name");
    let linked = filesystem
        .hard_link(raw_file.number, root.number, raw_link_name)
        .expect("raw hard link failed");
    assert_eq!(linked.number, raw_file.number);
    assert_eq!(linked.links, 2);
    assert_eq!(
        filesystem
            .lookup_child(root.number, raw_link_name)
            .expect("raw hard-link lookup failed")
            .expect("raw hard-link entry missing")
            .number,
        raw_file.number
    );

    let payload = b"open-unlink through the owned core";
    filesystem
        .write_inode(raw_file.number, 0, payload)
        .expect("owned inode write failed");
    let first_unlink = filesystem
        .unlink(raw_directory.number, raw_file_name)
        .expect("first raw unlink failed");
    assert_eq!(first_unlink.inode, raw_file.number);
    assert_eq!(first_unlink.remaining_links, 1);
    assert!(!first_unlink.requires_reap());

    let final_unlink = filesystem
        .unlink(root.number, raw_link_name)
        .expect("final raw unlink failed");
    assert_eq!(final_unlink.inode, raw_file.number);
    assert!(final_unlink.requires_reap());
    assert!(
        filesystem
            .lookup_child(root.number, raw_link_name)
            .expect("post-unlink lookup failed")
            .is_none()
    );
    let mut unlinked_payload = [0u8; 34];
    let read = filesystem
        .read_inode(raw_file.number, 0, &mut unlinked_payload)
        .expect("zero-link inode read failed");
    assert_eq!(&unlinked_payload[..read], payload);

    let busy_unmount = filesystem
        .unmount()
        .expect_err("a mount with a live orphan must remain busy");
    assert_eq!(busy_unmount.kind(), Ext4ErrorKind::Busy);

    filesystem
        .reap_unlinked_inode(raw_file.number)
        .expect("explicit zero-link reap failed");
    let second_reap = filesystem
        .reap_unlinked_inode(raw_file.number)
        .expect_err("a reaped inode must not be reclaimed twice");
    assert_eq!(second_reap.kind(), Ext4ErrorKind::NotFound);
    filesystem.unmount().expect("owned unmount failed");
}

#[test]
fn owned_remount_tracks_block_validity_option() {
    let mut filesystem = owned_test_filesystem();
    let disabled = filesystem.options().with_block_validity(false);
    filesystem
        .remount(disabled)
        .expect("disable block validity");
    assert!(!filesystem.options().block_validity);

    let enabled = filesystem.options().with_block_validity(true);
    filesystem
        .remount(enabled)
        .expect("reenable block validity");
    assert!(filesystem.options().block_validity);
}

#[test]
fn owned_remount_transitions_between_read_write_and_read_only() {
    let mut filesystem = owned_test_filesystem();
    let root = filesystem.root_inode();
    let context = MutationContext::new(1000, 1001, 0, 0o022);
    let permissions = FilePermissions::new(0o644).expect("valid permissions");
    filesystem
        .create_regular_file(
            context,
            root,
            FileName::new(b"before-readonly").expect("valid name"),
            permissions,
        )
        .expect("create pending file before remount");

    let read_only = MountOptions {
        readonly: true,
        ..filesystem.options()
    };
    filesystem.remount(read_only).expect("remount read-only");
    assert_eq!(filesystem.options(), read_only);
    let error = filesystem
        .create_regular_file(
            context,
            root,
            FileName::new(b"while-readonly").expect("valid name"),
            permissions,
        )
        .expect_err("read-only remount must reject mutations");
    assert_eq!(error.kind(), Ext4ErrorKind::ReadOnly);

    let read_write = MountOptions {
        readonly: false,
        ..filesystem.options()
    };
    filesystem.remount(read_write).expect("remount read-write");
    assert_eq!(filesystem.options(), read_write);
    filesystem
        .create_regular_file(
            context,
            root,
            FileName::new(b"after-readwrite").expect("valid name"),
            permissions,
        )
        .expect("read-write remount must accept mutations");
}

#[test]
fn owned_remount_to_read_only_rolls_back_options_on_flush_failure() {
    let (mut filesystem, fail_flush) = owned_test_filesystem_with_flush_failure();
    let previous = filesystem.options();
    let read_only = MountOptions {
        readonly: true,
        ..previous
    };
    fail_flush.store(true, Ordering::SeqCst);

    let error = filesystem
        .remount(read_only)
        .expect_err("failed journal flush must abort remount");

    assert_eq!(error.kind(), Ext4ErrorKind::Io);
    assert_eq!(filesystem.options(), previous);
}

#[test]
fn owned_remount_rejects_read_write_on_read_only_device() {
    let device = IoOnlyDevice::from(TestBlockDevice::new(100 * 1024 * 1024));
    let device = format(
        device,
        SeparateClock(Cell::new(1_700_000_000)),
        MkfsOptions::default(),
    )
    .expect("mkfs failed")
    .into_read_only();
    let services = MountServices::new(
        SeparateClock(Cell::new(1_800_000_000)),
        UnavailableCapabilities,
        RecordingObserver::default(),
    );
    let previous = MountOptions::read_only_no_journal_replay();
    let mut filesystem = Ext4::mount(device, services, previous).expect("read-only mount failed");
    let read_write = MountOptions {
        readonly: false,
        ..previous
    };

    let error = filesystem
        .remount(read_write)
        .expect_err("read-only device must reject read-write remount");

    assert_eq!(error.kind(), Ext4ErrorKind::ReadOnly);
    assert_eq!(filesystem.options(), previous);
    filesystem
        .unmount()
        .expect("read-only unmount must not write to the device");
    let error = filesystem
        .remount(previous)
        .expect_err("an unmounted owner cannot be remounted in place");
    assert_eq!(error.kind(), Ext4ErrorKind::Busy);
}

#[test]
fn owned_read_only_read_does_not_update_atime() {
    let mut filesystem = owned_test_filesystem();
    let context = MutationContext::new(1000, 1001, 0, 0o022);
    let file = filesystem
        .create_regular_file(
            context,
            filesystem.root_inode(),
            FileName::new(b"readonly-atime").expect("valid name"),
            FilePermissions::new(0o644).expect("valid permissions"),
        )
        .expect("create file");
    filesystem
        .write_inode(file.number, 0, b"payload")
        .expect("write file");
    filesystem.sync().expect("sync file before remount");
    let before = filesystem.inode(file.number).expect("inspect before read");

    let read_only = MountOptions {
        readonly: true,
        ..filesystem.options()
    };
    filesystem.remount(read_only).expect("remount read-only");
    let mut output = [0; 7];
    assert_eq!(
        filesystem
            .read_inode(file.number, 0, &mut output)
            .expect("read from read-only mount"),
        output.len()
    );
    assert_eq!(&output, b"payload");
    let after = filesystem.inode(file.number).expect("inspect after read");
    assert_eq!(after.atime, before.atime);
}

#[test]
fn owned_unmount_rejects_later_mutation_and_sync() {
    let mut filesystem = owned_test_filesystem();
    let context = MutationContext::new(1000, 1001, 0, 0o022);
    let file = filesystem
        .create_regular_file(
            context,
            filesystem.root_inode(),
            FileName::new(b"unmounted-owner").expect("valid name"),
            FilePermissions::new(0o644).expect("valid permissions"),
        )
        .expect("create file");

    filesystem.unmount().expect("unmount filesystem");

    let write_error = filesystem
        .write_inode(file.number, 0, b"forbidden")
        .expect_err("an unmounted owner must reject mutation");
    assert_eq!(write_error.kind(), Ext4ErrorKind::Busy);
    let sync_error = filesystem
        .sync()
        .expect_err("an unmounted owner must reject sync");
    assert_eq!(sync_error.kind(), Ext4ErrorKind::Busy);
}

#[test]
fn owned_rename_noreplace_preserves_both_entries() {
    let mut filesystem = owned_test_filesystem();
    let context = MutationContext::new(1000, 1001, 7, 0o022);
    let root = filesystem.root_inode();
    let source_name = FileName::new(b"source").expect("valid source name");
    let target_name = FileName::new(b"target").expect("valid target name");
    let permissions = FilePermissions::new(0o644).expect("valid file permissions");
    let source = filesystem
        .create_regular_file(context, root, source_name, permissions)
        .expect("source create failed");
    let target = filesystem
        .create_regular_file(context, root, target_name, permissions)
        .expect("target create failed");

    let error = filesystem
        .rename(
            root,
            source_name,
            root,
            target_name,
            RenameOptions::NO_REPLACE,
        )
        .expect_err("NOREPLACE must reject an existing target");

    assert_eq!(error.kind(), Ext4ErrorKind::AlreadyExists);
    assert_eq!(
        filesystem
            .lookup_child(root, source_name)
            .expect("source lookup failed")
            .expect("source disappeared")
            .number,
        source.number
    );
    assert_eq!(
        filesystem
            .lookup_child(root, target_name)
            .expect("target lookup failed")
            .expect("target disappeared")
            .number,
        target.number
    );
}

#[test]
fn owned_rename_exchange_swaps_raw_names_across_directories() {
    let mut filesystem = owned_test_filesystem();
    let context = MutationContext::new(1000, 1001, 7, 0o022);
    let root = filesystem.root_inode();
    let directory_permissions = FilePermissions::new(0o755).expect("valid directory permissions");
    let file_permissions = FilePermissions::new(0o644).expect("valid file permissions");
    let left = filesystem
        .create_directory(
            context,
            root,
            FileName::new(b"left").expect("valid left name"),
            directory_permissions,
        )
        .expect("left directory create failed");
    let right = filesystem
        .create_directory(
            context,
            root,
            FileName::new(b"right").expect("valid right name"),
            directory_permissions,
        )
        .expect("right directory create failed");
    let left_name = FileName::new(&[b'l', 0xff]).expect("valid raw left name");
    let right_name = FileName::new(&[b'r', 0xfe]).expect("valid raw right name");
    let left_file = filesystem
        .create_regular_file(context, left.number, left_name, file_permissions)
        .expect("left file create failed");
    let right_file = filesystem
        .create_regular_file(context, right.number, right_name, file_permissions)
        .expect("right file create failed");

    let outcome = filesystem
        .rename(
            left.number,
            left_name,
            right.number,
            right_name,
            RenameOptions::EXCHANGE,
        )
        .expect("raw exchange failed");

    assert_eq!(outcome.replaced, None);
    assert_eq!(
        filesystem
            .lookup_child(left.number, left_name)
            .expect("left lookup failed")
            .expect("left entry disappeared")
            .number,
        right_file.number
    );
    assert_eq!(
        filesystem
            .lookup_child(right.number, right_name)
            .expect("right lookup failed")
            .expect("right entry disappeared")
            .number,
        left_file.number
    );
}

#[test]
fn owned_rename_directory_updates_dotdot_and_parent_links() {
    let mut filesystem = owned_test_filesystem();
    let context = MutationContext::new(1000, 1001, 7, 0o022);
    let root = filesystem.root_inode();
    let permissions = FilePermissions::new(0o755).expect("valid directory permissions");
    let old_parent = filesystem
        .create_directory(
            context,
            root,
            FileName::new(b"old-parent").expect("valid old parent name"),
            permissions,
        )
        .expect("old parent create failed");
    let new_parent = filesystem
        .create_directory(
            context,
            root,
            FileName::new(b"new-parent").expect("valid new parent name"),
            permissions,
        )
        .expect("new parent create failed");
    let source_name = FileName::new(b"source-directory").expect("valid source name");
    let moved_name = FileName::new(b"moved-directory").expect("valid moved name");
    let moved = filesystem
        .create_directory(context, old_parent.number, source_name, permissions)
        .expect("source directory create failed");
    let old_links = filesystem
        .inode(old_parent.number)
        .expect("old parent inspection failed")
        .links;
    let new_links = filesystem
        .inode(new_parent.number)
        .expect("new parent inspection failed")
        .links;

    let _ = filesystem
        .rename(
            old_parent.number,
            source_name,
            new_parent.number,
            moved_name,
            RenameOptions::REPLACE,
        )
        .expect("cross-parent directory rename failed");

    assert!(
        filesystem
            .lookup_child(old_parent.number, source_name)
            .expect("old lookup failed")
            .is_none()
    );
    assert_eq!(
        filesystem
            .lookup_child(new_parent.number, moved_name)
            .expect("new lookup failed")
            .expect("moved directory missing")
            .number,
        moved.number
    );
    assert_eq!(
        filesystem
            .lookup_child(
                moved.number,
                FileName::new(b"..").expect("valid parent entry name"),
            )
            .expect("parent entry lookup failed")
            .expect("parent entry missing")
            .number,
        new_parent.number
    );
    assert_eq!(
        filesystem
            .inode(old_parent.number)
            .expect("old parent inspection failed")
            .links,
        old_links - 1
    );
    assert_eq!(
        filesystem
            .inode(new_parent.number)
            .expect("new parent inspection failed")
            .links,
        new_links + 1
    );
}

#[test]
fn owned_rename_replacement_returns_reapable_target() {
    let mut filesystem = owned_test_filesystem();
    let context = MutationContext::new(1000, 1001, 7, 0o022);
    let root = filesystem.root_inode();
    let permissions = FilePermissions::new(0o644).expect("valid file permissions");
    let source_name = FileName::new(b"replacement-source").expect("valid source name");
    let target_name = FileName::new(b"replacement-target").expect("valid target name");
    let source = filesystem
        .create_regular_file(context, root, source_name, permissions)
        .expect("source create failed");
    let target = filesystem
        .create_regular_file(context, root, target_name, permissions)
        .expect("target create failed");

    let outcome = filesystem
        .rename(root, source_name, root, target_name, RenameOptions::REPLACE)
        .expect("replacement rename failed");
    let replaced = outcome.replaced.expect("target outcome missing");

    assert_eq!(replaced.inode, target.number);
    assert!(replaced.requires_reap());
    assert_eq!(
        filesystem
            .lookup_child(root, target_name)
            .expect("target lookup failed")
            .expect("renamed source missing")
            .number,
        source.number
    );
    assert_eq!(
        filesystem
            .inode(target.number)
            .expect("detached target must remain allocated")
            .links,
        0
    );
    filesystem
        .reap_unlinked_inode(target.number)
        .expect("replacement target reap failed");
    let error = filesystem
        .reap_unlinked_inode(target.number)
        .expect_err("reaped replacement target must not be reclaimed twice");
    assert_eq!(error.kind(), Ext4ErrorKind::NotFound);
}

#[test]
fn owned_rename_rejects_moving_directory_below_itself() {
    let mut filesystem = owned_test_filesystem();
    let context = MutationContext::new(1000, 1001, 7, 0o022);
    let root = filesystem.root_inode();
    let permissions = FilePermissions::new(0o755).expect("valid directory permissions");
    let source_name = FileName::new(b"ancestor").expect("valid source name");
    let source = filesystem
        .create_directory(context, root, source_name, permissions)
        .expect("source directory create failed");
    let child_name = FileName::new(b"descendant").expect("valid child name");
    let child = filesystem
        .create_directory(context, source.number, child_name, permissions)
        .expect("child directory create failed");
    let nested_name = FileName::new(b"nested-ancestor").expect("valid nested name");

    let error = filesystem
        .rename(
            root,
            source_name,
            child.number,
            nested_name,
            RenameOptions::REPLACE,
        )
        .expect_err("moving a directory below itself must fail");

    assert_eq!(error.kind(), Ext4ErrorKind::InvalidInput);
    assert_eq!(
        filesystem
            .lookup_child(root, source_name)
            .expect("source lookup failed")
            .expect("source disappeared")
            .number,
        source.number
    );
    assert!(
        filesystem
            .lookup_child(child.number, nested_name)
            .expect("nested lookup failed")
            .is_none()
    );
}

#[test]
fn observer_receives_typed_mount_and_unmount_transitions() {
    let device = TestBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut observer = RecordingObserver::default();
    let mut fs = Ext4FileSystem::mount_with_options_and_observer(
        &mut jbd2_dev,
        MountOptions::read_write(),
        &mut observer,
    )
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
fn owned_special_inode_persists_modern_device_number() {
    let mut filesystem = owned_test_filesystem();
    let root = filesystem.root_inode();
    let context = MutationContext::new(1000, 1001, 0, 0o022);
    let device = DeviceNumber::new(259, 65_537).expect("valid modern device number");

    let inode = filesystem
        .create_special_inode(
            context,
            root,
            FileName::new(b"modern-device").expect("valid raw name"),
            FilePermissions::new(0o666).expect("valid permissions"),
            SpecialInodeKind::CharacterDevice(device),
        )
        .expect("special inode creation failed");

    assert_eq!(inode.file_type(), DirectoryEntryType::CharacterDevice);
    assert_eq!(inode.mode & 0o777, 0o644);
    assert_eq!(inode.uid, 1000);
    assert_eq!(inode.gid, 1001);
    assert_eq!(inode.device_number, Some(device));
    assert_eq!(inode.size, 0);
    assert_eq!(inode.blocks, 0);
}

#[test]
fn owned_empty_metadata_update_is_a_noop() {
    let mut filesystem = owned_test_filesystem();
    let root = filesystem.root_inode();
    let context = MutationContext::new(1000, 1001, 0, 0);
    let inode = filesystem
        .create_regular_file(
            context,
            root,
            FileName::new(b"metadata-noop").expect("valid raw name"),
            FilePermissions::new(0o644).expect("valid permissions"),
        )
        .expect("create regular file");

    let updated = filesystem
        .update_inode_metadata(inode.number, InodeMetadataUpdate::default())
        .expect("empty metadata update");

    assert_eq!(updated, inode);
    assert_eq!(
        filesystem.inode(inode.number).expect("inspect inode"),
        inode
    );
}

#[test]
fn owned_metadata_update_exposes_typed_flags_and_project_id() {
    let mut filesystem = owned_test_filesystem();
    let root = filesystem.root_inode();
    let context = MutationContext::new(1000, 1001, 0, 0);
    let inode = filesystem
        .create_regular_file(
            context,
            root,
            FileName::new(b"metadata-flags").expect("valid raw name"),
            FilePermissions::new(0o644).expect("valid permissions"),
        )
        .expect("create regular file");
    assert_eq!(inode.project_id, 0);
    assert!(inode.flags.contains(InodeFlags::EXTENTS));

    let default_project = filesystem
        .update_inode_metadata(
            inode.number,
            InodeMetadataUpdate {
                project_id: Some(0),
                ..Default::default()
            },
        )
        .expect("default project ID is a no-op without the project feature");
    assert_eq!(default_project, inode);

    let requested = InodeFlags::NO_DUMP | InodeFlags::NO_ATIME;
    let updated = filesystem
        .update_inode_metadata(
            inode.number,
            InodeMetadataUpdate {
                flags: Some(requested),
                project_id: Some(0),
                ..Default::default()
            },
        )
        .expect("update user-visible inode flags");
    assert!(updated.flags.contains(InodeFlags::EXTENTS));
    assert!(updated.flags.contains(requested));
    assert_eq!(updated.project_id, 0);

    let error = filesystem
        .update_inode_metadata(
            inode.number,
            InodeMetadataUpdate {
                project_id: Some(7),
                ..Default::default()
            },
        )
        .expect_err("non-default project ID requires the project feature");
    assert_eq!(error.kind(), Ext4ErrorKind::Unsupported);
    let unchanged = filesystem.inode(inode.number).expect("inspect inode");
    assert_eq!(unchanged.project_id, 0);
    assert!(unchanged.flags.contains(requested));
}

#[test]
fn owned_rmdir_keeps_open_directory_on_orphan_chain_until_reap() {
    let mut filesystem = owned_test_filesystem();
    let root = filesystem.root_inode();
    let context = MutationContext::new(1000, 1001, 0, 0);
    let permissions = FilePermissions::new(0o755).expect("valid permissions");
    let before_parent_links = filesystem.inode(root).expect("inspect root").links;
    let directory = filesystem
        .create_directory(
            context,
            root,
            FileName::new(b"open-directory").expect("valid raw name"),
            permissions,
        )
        .expect("create directory");

    let outcome = filesystem
        .remove_empty_directory(
            root,
            FileName::new(b"open-directory").expect("valid raw name"),
        )
        .expect("remove empty directory");
    assert_eq!(outcome.inode, directory.number);
    assert!(outcome.requires_reap());
    assert!(
        filesystem
            .lookup_child(
                root,
                FileName::new(b"open-directory").expect("valid raw name")
            )
            .expect("lookup removed directory")
            .is_none()
    );
    let unlinked = filesystem
        .inode(directory.number)
        .expect("open directory inode must remain allocated");
    assert_eq!(unlinked.links, 0);
    assert_eq!(unlinked.size, 0);
    assert_eq!(
        filesystem.inode(root).expect("inspect updated root").links,
        before_parent_links
    );

    filesystem
        .reap_unlinked_inode(directory.number)
        .expect("reap released directory");
    assert_eq!(
        filesystem.inode(directory.number).unwrap_err().kind(),
        Ext4ErrorKind::NotFound
    );
}

#[test]
fn owned_rmdir_rejects_nonempty_directory_without_mutation() {
    let mut filesystem = owned_test_filesystem();
    let root = filesystem.root_inode();
    let context = MutationContext::new(1000, 1001, 0, 0);
    let directory = filesystem
        .create_directory(
            context,
            root,
            FileName::new(b"nonempty").expect("valid raw name"),
            FilePermissions::new(0o755).expect("valid permissions"),
        )
        .expect("create directory");
    filesystem
        .create_regular_file(
            context,
            directory.number,
            FileName::new(b"child").expect("valid raw name"),
            FilePermissions::new(0o644).expect("valid permissions"),
        )
        .expect("create child");
    let parent_before = filesystem.inode(root).expect("inspect root");
    let directory_before = filesystem
        .inode(directory.number)
        .expect("inspect directory");

    let error = filesystem
        .remove_empty_directory(root, FileName::new(b"nonempty").expect("valid raw name"))
        .expect_err("nonempty directory must not be removed");
    assert_eq!(error.kind(), Ext4ErrorKind::NotEmpty);
    assert_eq!(filesystem.inode(root).expect("inspect root"), parent_before);
    assert_eq!(
        filesystem
            .inode(directory.number)
            .expect("inspect directory"),
        directory_before
    );
    assert!(
        filesystem
            .lookup_child(root, FileName::new(b"nonempty").expect("valid raw name"))
            .expect("lookup directory")
            .is_some()
    );
}

#[test]
fn owned_symlink_create_uses_linux_fast_boundary() {
    let mut filesystem = owned_test_filesystem();
    let root = filesystem.root_inode();
    let context = MutationContext::new(1000, 1001, 0, 0);
    let target_59 = [b'a'; 59];
    let target_60 = [b'b'; 60];

    let fast = filesystem
        .create_symlink(
            context,
            root,
            FileName::new(b"fast-link").expect("valid raw name"),
            &target_59,
        )
        .expect("create fast symlink");
    let long = filesystem
        .create_symlink(
            context,
            root,
            FileName::new(b"long-link").expect("valid raw name"),
            &target_60,
        )
        .expect("create long symlink");
    assert_eq!(fast.blocks, 0);
    assert_ne!(long.blocks, 0);

    let mut output = [0u8; 80];
    let read = filesystem
        .read_inode(fast.number, 0, &mut output)
        .expect("read fast symlink");
    assert_eq!(&output[..read], &target_59);
    let read = filesystem
        .read_inode(long.number, 0, &mut output)
        .expect("read long symlink");
    assert_eq!(&output[..read], &target_60);
}

#[test]
fn test_basic_mount_mkfs() {
    let device = TestBlockDevice::new(100 * 1024 * 1024); // 100MB
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

    // Test idea: create a fresh filesystem, perform one full create/read cycle,
    // and then unmount cleanly to prove the basic happy path works.
    mkfs(&mut jbd2_dev).expect("mkfs failed");

    let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

    mkdir(&mut jbd2_dev, &mut fs, "/test").expect("mkdir failed");

    let data = b"Hello, world!";
    mkfile(&mut jbd2_dev, &mut fs, "/test/hello.txt", Some(data), None).expect("mkfile failed");

    let read_data = read_file(&mut jbd2_dev, &mut fs, "/test/hello.txt").expect("read_file failed");
    assert_eq!(read_data, data.to_vec());

    umount(fs, &mut jbd2_dev).expect("umount failed");
}

#[test]
fn special_inode_does_not_initialize_an_extent_tree() {
    let device = TestBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

    mkfile_with_owner(
        &mut jbd2_dev,
        &mut fs,
        "/character-device",
        None,
        Some(rsext4::entries::Ext4DirEntry2::EXT4_FT_CHRDEV),
        1000,
        1001,
    )
    .expect("character device creation failed");
    let (_, inode) = rsext4::loopfile::get_file_inode(&mut fs, &mut jbd2_dev, "/character-device")
        .expect("character device lookup failed")
        .expect("character device missing");

    assert_eq!(
        inode.i_mode & rsext4::disknode::Ext4Inode::S_IFMT,
        rsext4::disknode::Ext4Inode::S_IFCHR
    );
    assert_eq!(
        inode.i_flags & rsext4::disknode::Ext4Inode::EXT4_EXTENTS_FL,
        0,
        "special inodes must not interpret i_block as an extent tree"
    );
    assert_eq!(inode.i_block, [0; 15]);
    assert_eq!(inode.size(), 0);
    assert_eq!(inode.i_blocks_lo, 0);
    assert_eq!(inode.l_i_blocks_high, 0);

    let error = mkfile(
        &mut jbd2_dev,
        &mut fs,
        "/invalid-special-payload",
        Some(b"not device data"),
        Some(rsext4::entries::Ext4DirEntry2::EXT4_FT_CHRDEV),
    )
    .expect_err("special inode payload must be rejected");
    assert_eq!(error.kind(), Ext4ErrorKind::InvalidInput);
    assert!(
        rsext4::loopfile::get_file_inode(&mut fs, &mut jbd2_dev, "/invalid-special-payload",)
            .expect("invalid special inode lookup failed")
            .is_none()
    );
}

#[test]
fn overlong_directory_name_is_rejected_without_truncation() {
    let device = TestBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");
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
    let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

    // Test idea: exercise path-based setup and I/O helpers together.
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

    umount(fs, &mut jbd2_dev).expect("umount failed");
}

#[test]
fn large_sequential_write_uses_one_contiguous_extent() {
    let device = TestBlockDevice::new(128 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);

    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let mut fs = Ext4FileSystem::mount(&mut jbd2_dev).expect("mount failed");

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

    let mut inode = fs
        .find_file(&mut jbd2_dev, "/large/run.bin")
        .expect("find failed");
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
