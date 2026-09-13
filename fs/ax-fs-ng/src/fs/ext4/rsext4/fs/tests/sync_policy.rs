//! Persistence policies exercise real VFS operations and real ext4 barriers.

use axfs_ng_vfs::{
    Location, MetadataUpdate, Mountpoint, NodePermission, NodeType, OpenOptions, WritebackPolicy,
};
use rsext4::{InodeFlags, InodeMetadataUpdate};

use super::*;

#[test]
fn ordinary_namespace_and_metadata_changes_do_not_force_a_commit() {
    let (filesystem, root, flushes) = background_mount(WritebackPolicy::empty());
    let before = flushes.load(Ordering::Relaxed);
    let file = create_file(&root, "original");
    file.update_metadata(MetadataUpdate {
        mode: Some(NodePermission::from_bits_truncate(0o640)),
        ..Default::default()
    })
    .unwrap();
    let link = root.link("linked", &file).unwrap();
    root.rename("linked", &root, "renamed").unwrap();
    root.unlink("renamed", false).unwrap();
    let symlink = root
        .create_symlink("symbolic", "original", NodePermission::default(), 0, 0)
        .unwrap();
    assert_eq!(flushes.load(Ordering::Relaxed), before);
    assert_eq!(file.metadata().unwrap().mode.bits(), 0o640);
    filesystem.sync_to_disk().unwrap();
    assert!(flushes.load(Ordering::Relaxed) > before);
    drop((link, symlink));
}

#[test]
fn synchronous_inode_metadata_and_directory_flags_force_commit() {
    let (filesystem, root, flushes) = background_mount(WritebackPolicy::empty());
    set_inode_flags(&filesystem, &root, InodeFlags::DIRECTORY_SYNC);
    let before = flushes.load(Ordering::Relaxed);
    let file = create_file(&root, "synchronous-parent");
    assert!(flushes.load(Ordering::Relaxed) > before);

    set_inode_flags(&filesystem, &file, InodeFlags::SYNC);
    let before = flushes.load(Ordering::Relaxed);
    file.update_metadata(MetadataUpdate {
        mode: Some(NodePermission::from_bits_truncate(0o600)),
        ..Default::default()
    })
    .unwrap();
    assert!(flushes.load(Ordering::Relaxed) > before);
    assert!(
        file.writeback_policy()
            .unwrap()
            .contains(WritebackPolicy::SYNCHRONOUS)
    );
}

#[test]
fn mount_directory_sync_applies_to_create_but_not_opening_existing_entries() {
    let (filesystem, root, flushes) = background_mount(WritebackPolicy::DIRECTORY_SYNC);
    let options = OpenOptions {
        create: true,
        ..Default::default()
    };
    let before = flushes.load(Ordering::Relaxed);
    let file = root.open_file("created", &options).unwrap();
    assert!(flushes.load(Ordering::Relaxed) > before);
    let before = flushes.load(Ordering::Relaxed);
    let reopened = root.open_file("created", &options).unwrap();
    assert_eq!(flushes.load(Ordering::Relaxed), before);
    assert_eq!(file.inode(), reopened.inode());
    filesystem.sync_to_disk().unwrap();
}

#[test]
fn bind_mounts_share_remount_sync_without_changing_directory_sync() {
    let (filesystem, root, _) = background_mount(WritebackPolicy::DIRECTORY_SYNC);
    let target = root
        .create("bind", NodeType::Directory, NodePermission::default(), 0, 0)
        .unwrap();
    let bound = target.bind_mount(&root, false).unwrap();
    assert_eq!(
        bound.filesystem_writeback_policy(),
        WritebackPolicy::DIRECTORY_SYNC
    );
    bound.set_filesystem_synchronous(true);
    assert_eq!(
        root.mountpoint().filesystem_writeback_policy(),
        WritebackPolicy::all()
    );
    root.mountpoint().set_filesystem_synchronous(false);
    assert_eq!(
        bound.filesystem_writeback_policy(),
        WritebackPolicy::DIRECTORY_SYNC
    );
    filesystem.sync_to_disk().unwrap();
}

#[test]
fn no_background_runtime_keeps_synchronous_namespace_completion() {
    let (mut filesystem, flushes) = test_filesystem(false);
    let filesystem = Arc::new_cyclic(|weak| {
        filesystem.self_ref = weak.clone();
        filesystem
    });
    let root = Mountpoint::new_root(&Filesystem::new(filesystem.clone())).root_location();
    let before = flushes.load(Ordering::Relaxed);
    let file = create_file(&root, "fallback");
    assert!(flushes.load(Ordering::Relaxed) > before);
    assert_eq!(file.len(), Ok(0));
}

#[test]
fn open_unlinked_hardlink_reuses_dirty_cache_until_final_inode_retirement() {
    crate::os::memory::test_support::with_test_page_provider(true, |_| {
        let (filesystem, root, _) = background_mount(WritebackPolicy::empty());
        let file = create_file(&root, "original");
        let alias = root.link("alias", &file).unwrap();
        let cached = crate::file::CachedFile::get_or_create(file.clone()).unwrap();
        cached.write_at(b"retained".as_slice(), 0).unwrap();
        root.unlink("original", false).unwrap();
        root.unlink("alias", false).unwrap();

        let reopened = crate::file::CachedFile::get_or_create(alias).unwrap();
        assert!(cached.ptr_eq(&reopened));
        let mut bytes = [0; 8];
        assert_eq!(reopened.read_at(&mut bytes[..], 0), Ok(8));
        assert_eq!(&bytes, b"retained");
        assert_eq!(file.metadata().unwrap().nlink, 0);
        cached.sync(false).unwrap();
        filesystem.sync_to_disk().unwrap();
    });
}

pub(super) fn background_mount(
    policy: WritebackPolicy,
) -> (Arc<Ext4Filesystem>, Location, Arc<AtomicUsize>) {
    let (mut filesystem, flushes) = test_filesystem(false);
    filesystem
        .lock()
        .ext4
        .enable_background_writeback()
        .unwrap();
    filesystem.writeback = Writeback::new(true);
    let filesystem = Arc::new_cyclic(|weak| {
        filesystem.self_ref = weak.clone();
        filesystem
    });
    let vfs = Filesystem::new(filesystem.clone());
    vfs.set_writeback_policy(policy);
    let root = Mountpoint::new_root(&vfs).root_location();
    filesystem.sync_to_disk().unwrap();
    (filesystem, root, flushes)
}

fn create_file(root: &Location, name: &str) -> Location {
    root.create(name, NodeType::RegularFile, NodePermission::default(), 0, 0)
        .unwrap()
}

fn set_inode_flags(filesystem: &Ext4Filesystem, location: &Location, flags: InodeFlags) {
    let number = InodeNumber::new(location.inode() as u32).unwrap();
    filesystem
        .with_writeback_progress(|state| {
            let current = state.ext4.inode(number)?.flags;
            state.ext4.update_inode_metadata(
                number,
                InodeMetadataUpdate {
                    flags: Some(current | flags),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    filesystem.sync_to_disk().unwrap();
}
