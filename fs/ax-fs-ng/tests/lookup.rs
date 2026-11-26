#![cfg(feature = "host-test")]

use ax_fs_ng::{
    FsContext, LookupPolicy, LookupRestrictions, LookupScope, MemoryFs, NativeLinkCaller,
    file::OpenOptions,
};
use ax_runtime as _;
use axfs_ng_vfs::{
    CachedLookup, CachedLookupContext, CachedLookupOptions, Mountpoint, MutationCredentials,
    NodePermission, NodeType, PathHandle, PathObservation, VfsError,
};

#[test]
fn resolved_paths_hold_the_selected_mount_until_the_consumer_retires() {
    ax_block::os::task::install_test_runtime_ops();
    let filesystem = MemoryFs::new();
    let mount = Mountpoint::new_root(&filesystem);
    let root = mount.root_location();
    let credentials = MutationCredentials::root();
    let mode = NodePermission::from_bits_truncate(0o700);
    let target = root
        .create("mounted", NodeType::Directory, mode, 0, 0, &credentials)
        .unwrap();
    let mounted = target.mount(&MemoryFs::new()).unwrap();
    mounted
        .root_location()
        .create("file", NodeType::RegularFile, mode, 0, 0, &credentials)
        .unwrap();
    let paths = FsContext::from_tree(mount, root).unwrap();
    let caller = NativeLinkCaller::root();
    let view = ax_fs_ng::FsView::native(paths, &caller);
    let selected = view.resolve("mounted/file").unwrap();
    assert_eq!(
        mounted.root_location().unmount(),
        Err(VfsError::ResourceBusy)
    );
    drop(selected);
    mounted.root_location().unmount().unwrap();
}

#[test]
fn open_keeps_one_link_budget_through_the_final_target_and_creation() {
    ax_block::os::task::install_test_runtime_ops();
    let filesystem = MemoryFs::new();
    let mount = Mountpoint::new_root(&filesystem);
    // The shared link budget is independent of automatic timestamp work.
    // Mountpoint currently accepts Linux MS_* option bits (MS_NOATIME).
    mount.set_mount_flags(1 << 10);
    let root = mount.root_location();
    let credentials = MutationCredentials::root();
    let mode = NodePermission::from_bits_truncate(0o700);
    let directory = root
        .create("real", NodeType::Directory, mode, 0, 0, &credentials)
        .unwrap();
    let expected = directory
        .create("existing", NodeType::RegularFile, mode, 0, 0, &credentials)
        .unwrap();
    directory
        .create_symlink("present", "existing", mode, 0, 0, &credentials)
        .unwrap();
    directory
        .create_symlink("dangling", "created", mode, 0, 0, &credentials)
        .unwrap();
    for count in 1..=40 {
        let target = if count == 1 {
            "real".into()
        } else {
            format!("parent-{}", count - 1)
        };
        root.create_symlink(
            &format!("parent-{count}"),
            &target,
            mode,
            0,
            0,
            &credentials,
        )
        .unwrap();
    }
    let context = FsContext::from_tree(mount, root).unwrap();
    let context_caller = NativeLinkCaller::root();
    let context = ax_fs_ng::FsView::native(context, &context_caller);
    let mut read = OpenOptions::new();
    read.read(true);
    let mut create = OpenOptions::new();
    create.write(true).create(true);

    // Reject the excessive path before the successful creation can populate
    // its target. Both operations use the real native namespace and opener.
    for count in [40, 39] {
        let existing = read
            .open(&context, format!("parent-{count}/present"))
            .and_then(|opened| opened.into_file())
            .map(|file| file.location().inode());
        let created = create
            .open(&context, format!("parent-{count}/dangling"))
            .and_then(|opened| opened.into_file())
            .map(|file| file.location().inode());
        let cached = retry_lookup_contention(|| {
            CachedLookup::new(CachedLookupContext {
                root: context.root(),
                start: context.cwd(),
                credentials: *context.credentials(),
                policy: LookupPolicy::default(),
            })?
            .resolve(
                &format!("parent-{count}/present"),
                CachedLookupOptions {
                    follow_final_link: true,
                    require_directory: false,
                },
            )
        })
        .map(|path| path.location().inode());
        if count == 40 {
            assert_eq!(existing, Err(VfsError::FilesystemLoop));
            assert_eq!(created, Err(VfsError::FilesystemLoop));
            assert_eq!(cached, Err(VfsError::FilesystemLoop));
            assert!(matches!(
                directory.lookup_no_follow("created"),
                Err(VfsError::NotFound)
            ));
        } else {
            assert_eq!(existing, Ok(expected.inode()));
            assert_eq!(cached, Ok(expected.inode()));
            assert_eq!(
                created,
                Ok(directory.lookup_no_follow("created").unwrap().inode())
            );
        }
    }
}

#[test]
fn scoped_lookup_rejects_a_moved_parent_and_revalidates_the_actual_create_slot() {
    ax_block::os::task::install_test_runtime_ops();
    let filesystem = MemoryFs::new();
    let mount = Mountpoint::new_root(&filesystem);
    let root = mount.root_location();
    let credentials = MutationCredentials::root();
    let mode = NodePermission::from_bits_truncate(0o700);
    let scope = root
        .create("scope", NodeType::Directory, mode, 0, 0, &credentials)
        .unwrap();
    root.create("outside", NodeType::Directory, mode, 0, 0, &credentials)
        .unwrap();
    let branch = scope
        .create("branch", NodeType::Directory, mode, 0, 0, &credentials)
        .unwrap();
    let leaf = branch
        .create("leaf", NodeType::Directory, mode, 0, 0, &credentials)
        .unwrap();
    let paths = FsContext::from_tree(mount, root).unwrap();
    let caller = NativeLinkCaller::root();
    let view = ax_fs_ng::FsView::native(paths, &caller);
    let scoped = view
        .with_dirfd(PathHandle::acquire(scope).unwrap())
        .unwrap()
        .with_lookup_policy(LookupPolicy::new(
            LookupScope::Beneath,
            LookupRestrictions::empty(),
        ));
    let moved = core::cell::Cell::new(false);
    let mut rejected = false;
    for _ in 0..1024 {
        let result = scoped.resolve_checked("branch/leaf/../..", |directory| {
            if directory.ptr_eq(&leaf) && !moved.replace(true) {
                // The actual MemoryFs rename completes after search admission
                // and before the same operation follows its first `..`.
                view.rename("scope/branch", "outside/branch")?;
            }
            Ok(())
        });
        if !moved.get() && matches!(result, Err(VfsError::WouldBlock)) {
            std::thread::yield_now();
            continue;
        }
        assert!(moved.get(), "the controlled rename must execute");
        assert!(matches!(result, Err(VfsError::WouldBlock)));
        rejected = true;
        break;
    }
    assert!(
        rejected,
        "the controlled lookup must reach its rename window"
    );

    view.rename("outside/branch", "scope/branch").unwrap();
    let mut create = OpenOptions::new();
    create.write(true).create(true);
    let created = retry_lookup_contention(|| create.open(&scoped, "branch/created")).unwrap();
    assert_eq!(
        created.into_file().unwrap().location().inode(),
        branch.lookup_no_follow("created").unwrap().inode()
    );
    let observation = retry_lookup_contention(PathObservation::capture).unwrap();
    view.rename("scope/branch", "outside/branch").unwrap();
    let result = branch.open_file_with_status(
        "escaped",
        &axfs_ng_vfs::OpenOptions {
            create: true,
            observation: Some(observation),
            ..Default::default()
        },
        &credentials,
    );
    assert!(matches!(result, Err(VfsError::WouldBlock)));
    assert!(matches!(
        branch.lookup_no_follow("escaped"),
        Err(VfsError::NotFound)
    ));
}

fn retry_lookup_contention<T>(
    mut operation: impl FnMut() -> Result<T, VfsError>,
) -> Result<T, VfsError> {
    for _ in 0..1024 {
        let result = operation();
        if !matches!(result, Err(VfsError::WouldBlock)) {
            return result;
        }
        std::thread::yield_now();
    }
    Err(VfsError::WouldBlock)
}
