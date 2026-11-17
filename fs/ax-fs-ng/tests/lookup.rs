#![cfg(feature = "host-test")]

use ax_fs_ng::{FsContext, MemoryFs, file::OpenOptions};
use ax_runtime as _;
use axfs_ng_vfs::{Mountpoint, MutationCredentials, NodePermission, NodeType, VfsError};

#[test]
fn open_keeps_one_link_budget_through_the_final_target_and_creation() {
    ax_block::os::task::install_test_runtime_ops();
    let filesystem = MemoryFs::new();
    let mount = Mountpoint::new_root(&filesystem);
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
    let mut read = OpenOptions::new();
    read.read(true);
    let mut create = OpenOptions::new();
    create.write(true).create(true);

    // Reject the excessive path before the successful creation can populate
    // its target. Both operations use the real native namespace and opener.
    for count in [40, 39] {
        let existing = read
            .open_with_credentials(&context, format!("parent-{count}/present"), &credentials)
            .and_then(|opened| opened.into_file())
            .map(|file| file.location().inode());
        let created = create
            .open_with_credentials(&context, format!("parent-{count}/dangling"), &credentials)
            .and_then(|opened| opened.into_file())
            .map(|file| file.location().inode());
        if count == 40 {
            assert_eq!(existing, Err(VfsError::FilesystemLoop));
            assert_eq!(created, Err(VfsError::FilesystemLoop));
            assert!(matches!(
                directory.lookup_no_follow("created"),
                Err(VfsError::NotFound)
            ));
        } else {
            assert_eq!(existing, Ok(expected.inode()));
            assert_eq!(
                created,
                Ok(directory.lookup_no_follow("created").unwrap().inode())
            );
        }
    }
}
