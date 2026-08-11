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

type TestOwnedFilesystem = Ext4<
    IoOnlyDevice,
    MountedServices<
        UnavailableCapabilities,
        UnavailableCapabilities,
        UnavailableCapabilities,
        RecordingObserver,
    >,
>;

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
        UnavailableCapabilities,
        UnavailableCapabilities,
        RecordingObserver::default(),
    );
    Ext4::mount(device, services, MountOptions::read_write()).expect("owned mount failed")
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
        .hard_link(context, raw_file.number, root.number, raw_link_name)
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
        .write_inode(context, raw_file.number, 0, payload)
        .expect("owned inode write failed");
    let first_unlink = filesystem
        .unlink(context, raw_directory.number, raw_file_name)
        .expect("first raw unlink failed");
    assert_eq!(first_unlink.inode, raw_file.number);
    assert_eq!(first_unlink.remaining_links, 1);
    assert!(!first_unlink.requires_reap());

    let final_unlink = filesystem
        .unlink(context, root.number, raw_link_name)
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
            context,
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
            context,
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
            context,
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
        .rename(
            context,
            root,
            source_name,
            root,
            target_name,
            RenameOptions::REPLACE,
        )
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
            context,
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

    assert_eq!(
        inode.mode & rsext4::disknode::Ext4Inode::S_IFMT,
        rsext4::disknode::Ext4Inode::S_IFCHR
    );
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
        .update_inode_metadata(context, inode.number, InodeMetadataUpdate::default())
        .expect("empty metadata update");

    assert_eq!(updated, inode);
    assert_eq!(
        filesystem.inode(inode.number).expect("inspect inode"),
        inode
    );
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
            context,
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
        .remove_empty_directory(
            context,
            root,
            FileName::new(b"nonempty").expect("valid raw name"),
        )
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
fn owned_symlink_uses_linux_fast_boundary_and_replaces_target() {
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

    let replacement = [b'c'; 70];
    filesystem
        .set_symlink_target(context, fast.number, &replacement)
        .expect("replace fast symlink with long target");
    assert_ne!(
        filesystem
            .inode(fast.number)
            .expect("inspect replaced symlink")
            .blocks,
        0
    );
    let read = filesystem
        .read_inode(fast.number, 0, &mut output)
        .expect("read replaced long symlink");
    assert_eq!(&output[..read], &replacement);

    filesystem
        .set_symlink_target(context, fast.number, b"short")
        .expect("replace long symlink with fast target");
    assert_eq!(
        filesystem
            .inode(fast.number)
            .expect("inspect fast replacement")
            .blocks,
        0
    );
    let read = filesystem
        .read_inode(fast.number, 0, &mut output)
        .expect("read fast replacement");
    assert_eq!(&output[..read], b"short");
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
fn special_inode_does_not_initialize_an_extent_tree() {
    let device = TestBlockDevice::new(100 * 1024 * 1024);
    let mut jbd2_dev = Jbd2Dev::initial_jbd2dev(0, device, true);
    mkfs(&mut jbd2_dev).expect("mkfs failed");
    let mut fs = mount(&mut jbd2_dev).expect("mount failed");

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
