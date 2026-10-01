//! Real VFS metadata hits must not enter the mounted-filesystem lock path.

use core::cell::Cell;

use axfs_ng_vfs::{MetadataUpdate, NodeOps, NodePermission, NodeType};

use super::*;

std::thread_local! {
    static REJECT_EXT4_LOCK: Cell<bool> = const { Cell::new(false) };
    static EXT4_LOCK_REQUESTS: Cell<usize> = const { Cell::new(0) };
}

struct CacheOnlyInspection;

impl CacheOnlyInspection {
    fn begin() -> Self {
        REJECT_EXT4_LOCK.set(true);
        Self
    }
}

impl Drop for CacheOnlyInspection {
    fn drop(&mut self) {
        REJECT_EXT4_LOCK.set(false);
    }
}

pub(super) fn inspect_ext4_lock() {
    EXT4_LOCK_REQUESTS.set(EXT4_LOCK_REQUESTS.get() + 1);
    assert!(
        !REJECT_EXT4_LOCK.get(),
        "cache-only metadata entered ext4 lock acquisition"
    );
}

#[test]
fn metadata_miss_prepares_and_publishes_before_the_next_cache_only_hit() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    let input = super::read::create_inode(&filesystem, b"input", b"hello");
    let number = InodeNumber::new(input.inode() as u32).unwrap();
    // Evict through real cache pressure, without exposing a test-only eviction
    // control in the mounted filesystem API. Each created inode is synced.
    for index in 0..=rsext4::config::INODE_CACHE_MAX {
        let name = alloc::format!("pressure-{index}");
        super::read::create_inode(&filesystem, name.as_bytes(), b"");
    }
    assert!(filesystem.inode_metadata.try_get(number).unwrap().is_none());
    let before = EXT4_LOCK_REQUESTS.get();

    assert_eq!(input.metadata().unwrap().size, 5);

    // Prepare and validated publication each use a short mount section; the
    // independent read between them must not be folded back under that lock.
    assert_eq!(EXT4_LOCK_REQUESTS.get(), before + 2);
    let _state = filesystem.lock();
    let _cache_only = CacheOnlyInspection::begin();
    assert_eq!(input.metadata().unwrap().size, 5);
    assert_eq!(input.len(), Ok(5));
}

#[test]
fn cached_vfs_metadata_and_length_complete_while_ext4_is_locked() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    let input = super::read::create_inode(&filesystem, b"input", b"hello");
    let expected = input.metadata().unwrap();
    let _state = filesystem.lock();
    let _cache_only = CacheOnlyInspection::begin();

    let metadata = input.metadata().unwrap();
    assert_eq!(metadata.size, 5);
    assert_eq!(metadata.mode.bits(), expected.mode.bits());
    assert_eq!(metadata.blocks, expected.blocks);
    assert_eq!(input.len(), Ok(5));
}

#[test]
fn repeated_missing_directory_lookup_completes_while_ext4_is_locked() {
    let (filesystem, root, _) = super::sync_policy::background_mount();
    let directory = root.entry().as_dir().unwrap();
    assert!(matches!(
        directory.lookup("absent"),
        Err(VfsError::NotFound)
    ));
    let _state = filesystem.lock();
    // Reject acquisition before a regression could block recursively on the
    // held filesystem mutex.
    let _cache_only = CacheOnlyInspection::begin();
    assert!(matches!(
        directory.lookup("absent"),
        Err(VfsError::NotFound)
    ));
}

#[test]
fn negative_directory_cache_follows_create_link_symlink_and_unlink() {
    let (_, root, _) = super::sync_policy::background_mount();
    let directory = root.entry().as_dir().unwrap();
    for name in ["created", "linked", "symbolic"] {
        assert!(matches!(directory.lookup(name), Err(VfsError::NotFound)));
    }
    let created = directory
        .create(
            "created",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    assert_eq!(
        directory.lookup("created").unwrap().inode(),
        created.inode()
    );
    let linked = directory.link("linked", &created).unwrap();
    assert_eq!(directory.lookup("linked").unwrap().inode(), created.inode());
    let symbolic = directory
        .create_symlink("symbolic", "created", NodePermission::default(), 0, 0)
        .unwrap();
    assert_eq!(
        directory.lookup("symbolic").unwrap().inode(),
        symbolic.inode()
    );
    directory.unlink("linked", false).unwrap();
    assert!(matches!(
        directory.lookup("linked"),
        Err(VfsError::NotFound)
    ));
    directory.link("linked", &created).unwrap();
    assert_eq!(directory.lookup("linked").unwrap().inode(), linked.inode());
}

#[test]
fn negative_directory_cache_is_revalidated_through_a_renamed_open_directory() {
    let (_, root, _) = super::sync_policy::background_mount();
    let original = root
        .create(
            "original",
            NodeType::Directory,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    root.rename("original", &root, "renamed").unwrap();
    let renamed = root.lookup_no_follow("renamed").unwrap();
    assert!(!original.entry().ptr_eq(renamed.entry()));
    assert_eq!(original.inode(), renamed.inode());
    assert!(matches!(
        original.lookup_no_follow("created"),
        Err(VfsError::NotFound)
    ));
    let created = renamed
        .create(
            "created",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    assert_eq!(
        original.lookup_no_follow("created").unwrap().inode(),
        created.inode()
    );
}

#[test]
fn negative_directory_cache_follows_rename_and_preserves_unsupported_whiteout() {
    use axfs_ng_vfs::RenameOptions;

    let (_, root, _) = super::sync_policy::background_mount();
    let target = root
        .create(
            "target",
            NodeType::Directory,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    let original = root
        .create(
            "original",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    assert!(matches!(
        target.lookup_no_follow("moved"),
        Err(VfsError::NotFound)
    ));
    root.rename("original", &target, "moved").unwrap();
    assert_eq!(
        target.lookup_no_follow("moved").unwrap().inode(),
        original.inode()
    );
    assert!(matches!(
        root.lookup_no_follow("original"),
        Err(VfsError::NotFound)
    ));
    // The current portable core explicitly rejects rename:whiteout. Preserve
    // that error and both names rather than expanding backend capabilities.
    assert_eq!(
        target.rename_with_options("moved", &root, "original", RenameOptions::WHITEOUT),
        Err(VfsError::OperationNotSupported)
    );
    assert!(matches!(
        root.lookup_no_follow("original"),
        Err(VfsError::NotFound)
    ));
    assert_eq!(
        target.lookup_no_follow("moved").unwrap().inode(),
        original.inode()
    );
    target.rename("moved", &root, "original").unwrap();
    assert_eq!(
        root.lookup_no_follow("original").unwrap().inode(),
        original.inode()
    );
    assert!(matches!(
        target.lookup_no_follow("moved"),
        Err(VfsError::NotFound)
    ));
}

#[test]
fn directory_exchange_preserves_both_cached_file_owners() {
    use axfs_ng_vfs::RenameOptions;

    let (_, root, _) = super::sync_policy::background_mount();
    let directory = root.entry().as_dir().unwrap();
    let first = directory
        .create(
            "first",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    let second = directory
        .create(
            "second",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    first.user_data().insert(1_u32);
    second.user_data().insert(2_u32);
    directory
        .rename_with_options("first", directory, "second", RenameOptions::EXCHANGE)
        .unwrap();
    let new_first = directory.lookup("first").unwrap();
    let new_second = directory.lookup("second").unwrap();
    assert_eq!(new_first.inode(), second.inode());
    assert_eq!(new_second.inode(), first.inode());
    assert_eq!(*new_first.user_data().get::<u32>().unwrap(), 2);
    assert_eq!(*new_second.user_data().get::<u32>().unwrap(), 1);
}

#[test]
fn cached_metadata_observes_committed_changes_and_open_unlinked_lifetime() {
    let (filesystem, _) = test_filesystem(false);
    let filesystem = Arc::new(filesystem);
    let input = super::read::create_inode(&filesystem, b"input", b"hello");
    let number = InodeNumber::new(input.inode() as u32).unwrap();
    {
        let mut state = filesystem.lock();
        state
            .ext4
            .update_inode_metadata(
                number,
                rsext4::InodeMetadataUpdate {
                    permissions: Some(rsext4::FilePermissions::new(0o640).unwrap()),
                    ..Default::default()
                },
            )
            .unwrap();
        let root = state.ext4.root_inode();
        let outcome = state
            .ext4
            .unlink(root, rsext4::FileName::new(b"input").unwrap())
            .unwrap();
        assert!(outcome.requires_reap());
        assert_eq!(state.publish_zero_link(number), None);
        state.ext4.inode(number).unwrap();
    }
    let _state = filesystem.lock();
    let _cache_only = CacheOnlyInspection::begin();
    let metadata = input.metadata().unwrap();
    assert_eq!(metadata.nlink, 0);
    assert_eq!(metadata.mode.bits(), 0o640);
    assert_eq!(input.len(), Ok(5));
}

#[test]
fn hardlink_metadata_updates_remain_visible_without_global_lock_after_unlink() {
    let (filesystem, root, _) = super::sync_policy::background_mount();
    let input = root
        .create(
            "input",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    let alias = root.link("alias", &input).unwrap();
    alias
        .update_metadata(MetadataUpdate {
            mode: Some(NodePermission::from_bits_truncate(0o640)),
            owner: Some((123_456, 234_567)),
            ..Default::default()
        })
        .unwrap();
    alias.entry().as_file().unwrap().set_len(7).unwrap();
    root.unlink("input", false).unwrap();
    root.unlink("alias", false).unwrap();
    let _state = filesystem.lock();
    let _cache_only = CacheOnlyInspection::begin();

    for location in [&input, &alias] {
        let metadata = location.metadata().unwrap();
        assert_eq!(metadata.mode.bits(), 0o640);
        assert_eq!((metadata.uid, metadata.gid), (123_456, 234_567));
        assert_eq!(metadata.nlink, 0);
        assert_eq!(metadata.size, 7);
        assert_eq!(location.len(), Ok(7));
    }
}
