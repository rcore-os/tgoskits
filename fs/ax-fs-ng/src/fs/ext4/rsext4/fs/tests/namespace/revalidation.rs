//! Changes admitted during pressure must be rechecked by the next attempt.

use super::*;

#[test]
fn create_retry_preserves_a_name_created_during_commit() {
    let (filesystem, root, _mount) = fixture();
    filesystem.lock().staging = true;
    let competing = root.clone();
    let probe = watch_flush(move || {
        create_file(&competing, "contested");
        Ok(())
    });

    assert_eq!(
        create_result(&root, "contested").unwrap_err(),
        VfsError::AlreadyExists
    );

    probe.finish();
    assert_eq!(
        root.lookup("contested").unwrap().metadata().unwrap().nlink,
        1
    );
    assert_admission_released(&filesystem);
}

#[test]
fn removed_retained_parent_cannot_publish_a_child_after_pressure() {
    let (filesystem, root, _mount) = fixture();
    let parent = root
        .create(
            "parent",
            NodeType::Directory,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    let parent: Arc<Inode> = parent.downcast().unwrap();
    filesystem.lock().staging = true;
    let competing = root.clone();
    let probe = watch_flush(move || {
        competing.unlink("parent", true).unwrap();
        Ok(())
    });

    assert_eq!(
        create_result(&parent, "unreachable").unwrap_err(),
        VfsError::NotFound
    );

    probe.finish();
    assert_eq!(parent.metadata().unwrap().nlink, 0);
    assert_eq!(
        parent.lookup("unreachable").unwrap_err(),
        VfsError::NotFound
    );
    assert_eq!(root.lookup("parent").unwrap_err(), VfsError::NotFound);
    assert_admission_released(&filesystem);
}

#[test]
fn rename_retry_re_resolves_a_replaced_source_name() {
    let (filesystem, root, mount) = fixture();
    let original = create_file(&root, "source");
    let replacement = create_file(&root, "replacement");
    let directory = filesystem.root_dir();
    filesystem.lock().staging = true;
    let competing = mount.root_location();
    let parent = directory.clone();
    let probe = watch_flush(move || {
        let directory = axfs_ng_vfs::Location::new(competing.mountpoint().clone(), parent);
        competing
            .rename_with_options(
                "source",
                &directory,
                "saved",
                RenameOptions::REPLACE,
                &axfs_ng_vfs::MutationCredentials::root(),
            )
            .unwrap();
        competing
            .rename_with_options(
                "replacement",
                &directory,
                "source",
                RenameOptions::REPLACE,
                &axfs_ng_vfs::MutationCredentials::root(),
            )
            .unwrap();
        Ok(())
    });

    mount
        .root_location()
        .rename_with_options(
            "source",
            &axfs_ng_vfs::Location::new(mount.clone(), directory.clone()),
            "target",
            RenameOptions::REPLACE,
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();

    probe.finish();
    assert_eq!(root.lookup("target").unwrap().inode(), replacement.inode());
    assert_eq!(root.lookup("saved").unwrap().inode(), original.inode());
    assert_eq!(root.lookup("source").unwrap_err(), VfsError::NotFound);
}

#[test]
fn rename_retry_rejects_a_removed_retained_destination_directory() {
    let (filesystem, root, mount) = fixture();
    let source = create_file(&root, "source");
    let destination = root
        .create(
            "destination",
            NodeType::Directory,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    filesystem.lock().staging = true;
    let competing = root.clone();
    let probe = watch_flush(move || {
        competing.unlink("destination", true).unwrap();
        Ok(())
    });

    assert_eq!(
        mount.root_location().rename_with_options(
            "source",
            &axfs_ng_vfs::Location::new(mount.clone(), destination.clone()),
            "target",
            RenameOptions::REPLACE,
            &axfs_ng_vfs::MutationCredentials::root(),
        ),
        Err(VfsError::NotFound)
    );

    probe.finish();
    assert_eq!(root.lookup("source").unwrap().inode(), source.inode());
    assert_eq!(destination.metadata().unwrap().nlink, 0);
}
