//! Persistence policies exercise real VFS operations and real ext4 barriers.

use axfs_ng_vfs::{
    Location, MetadataUpdate, Mountpoint, NodePermission, NodeType, WritebackPolicy,
};
use rsext4::{InodeFlags, InodeMetadataUpdate};

use super::*;
use crate::file::{File, FileBackend, FileFlags};

#[test]
fn ordinary_namespace_and_metadata_changes_do_not_force_a_commit() {
    let (filesystem, root, flushes) = background_mount();
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
    let (filesystem, root, flushes) = background_mount();
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
        file.entry()
            .writeback_policy()
            .unwrap()
            .contains(WritebackPolicy::SYNCHRONOUS)
    );

    let handle = File::new(FileBackend::new_direct(file), FileFlags::WRITE);
    let before = flushes.load(Ordering::Relaxed);
    assert_eq!(handle.write_at(b"persist".as_slice(), 0), Ok(7));
    assert!(flushes.load(Ordering::Relaxed) > before);
}

#[test]
fn synchronous_inode_cached_write_forces_commit() {
    crate::os::memory::test_support::with_test_page_provider(true, |_| {
        let (filesystem, root, flushes) = background_mount();
        let file = create_file(&root, "cached-sync");
        let initial = File::new(FileBackend::new_direct(file.clone()), FileFlags::WRITE);
        assert_eq!(initial.write_at(b"initial".as_slice(), 0), Ok(7));
        filesystem.sync_to_disk().unwrap();
        set_inode_flags(&filesystem, &file, InodeFlags::SYNC);
        let handle = File::new(
            FileBackend::new_cached(file).unwrap(),
            FileFlags::WRITE | FileFlags::APPEND,
        );
        let before = flushes.load(Ordering::Relaxed);
        let writing_filesystem = filesystem.clone();
        let probe = super::namespace::watch_flush(move || {
            assert_eq!(writing_filesystem.cached_writes.active_count(), 1);
            Ok(())
        });
        assert_eq!(handle.write_at(b"persist".as_slice(), 0), Ok(7));
        probe.finish();
        assert!(flushes.load(Ordering::Relaxed) > before);
        let FileBackend::Cached(cached) = handle.backend().unwrap() else {
            panic!("regular file should use the cached backend");
        };
        assert!(cached.dirty_pages_in_range(0, 1).unwrap().is_empty());

        let writing_filesystem = filesystem.clone();
        let probe = super::namespace::watch_flush(move || {
            assert_eq!(writing_filesystem.cached_writes.active_count(), 1);
            Ok(())
        });
        assert_eq!(handle.write(b"!".as_slice()), Ok(1));
        probe.finish();
        assert_eq!(handle.position(), Some(8));
        assert!(cached.dirty_pages_in_range(0, 1).unwrap().is_empty());
    });
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
        let (filesystem, root, _) = background_mount();
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

pub(super) fn background_mount() -> (Arc<Ext4Filesystem>, Location, Arc<AtomicUsize>) {
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
