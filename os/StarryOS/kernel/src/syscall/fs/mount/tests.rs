use alloc::sync::Arc;

use axfs_ng_vfs::{Location, Mountpoint, NodePermission, NodeType, UnmountKind, VfsError};

use crate::pseudofs::MemoryFs;

fn directory(parent: &Location, name: &str) -> Location {
    parent
        .create(
            name,
            NodeType::Directory,
            NodePermission::from_bits_truncate(0o755),
            0,
            0,
        )
        .expect("create mount directory")
}

fn mounted_tree() -> (Location, Arc<Mountpoint>) {
    let root = Mountpoint::new_root(&MemoryFs::new()).root_location();
    let mounted = directory(&root, "mnt")
        .mount(&MemoryFs::new())
        .expect("mount tmpfs");
    (root, mounted)
}

#[axtest::axtest]
fn admitted_unmount_survives_unrelated_namespace_clone() {
    let (root, mounted) = mounted_tree();
    let plan = mounted
        .plan_unmount(UnmountKind::Normal)
        .expect("admit unmount");

    // Reproduce the exact plan/commit interleaving without scheduling luck.
    // This is a separate tree with no propagation relationship to the target.
    let unrelated = Mountpoint::new_root(&MemoryFs::new());
    let cloned_namespace = unrelated.clone_tree();
    mounted
        .root_location()
        .commit_unmount(plan)
        .expect("unrelated namespace mutation must not cause EBUSY");

    assert!(!root.lookup_no_follow("mnt").unwrap().is_root_of_mount());
    assert!(mounted.location().is_none());
    assert!(cloned_namespace.is_root());
}

#[axtest::axtest]
fn admitted_unmount_rejects_moved_target() {
    let (root, mounted) = mounted_tree();
    let destination = directory(&root, "moved");
    let plan = mounted
        .plan_unmount(UnmountKind::Normal)
        .expect("admit unmount");
    mounted
        .root_location()
        .move_mount(&destination)
        .expect("move target");

    assert_eq!(
        mounted.root_location().commit_unmount(plan),
        Err(VfsError::ResourceBusy)
    );
    assert!(root.lookup_no_follow("moved").unwrap().is_root_of_mount());
    mounted
        .root_location()
        .unmount()
        .expect("unmount with fresh admission");
}

#[axtest::axtest]
fn admitted_unmount_rejects_new_propagation_target() {
    let (root, mounted) = mounted_tree();
    root.mountpoint().set_shared();
    let plan = mounted
        .plan_unmount(UnmountKind::Normal)
        .expect("admit unmount");
    let peer_namespace = root.mountpoint().clone_tree();

    assert_eq!(
        mounted.root_location().commit_unmount(plan),
        Err(VfsError::ResourceBusy)
    );
    assert!(root.lookup_no_follow("mnt").unwrap().is_root_of_mount());
    assert!(
        peer_namespace
            .root_location()
            .lookup_no_follow("mnt")
            .unwrap()
            .is_root_of_mount()
    );
    mounted
        .root_location()
        .unmount()
        .expect("unmount both peers with fresh admission");
}

#[axtest::axtest]
fn admitted_unmount_rejects_new_child_mount() {
    let (root, mounted) = mounted_tree();
    let child_dir = directory(&mounted.root_location(), "child");
    let plan = mounted
        .plan_unmount(UnmountKind::Normal)
        .expect("admit unmount");
    let child = child_dir.mount(&MemoryFs::new()).expect("mount child");

    assert_eq!(
        mounted.root_location().commit_unmount(plan),
        Err(VfsError::ResourceBusy)
    );
    assert!(root.lookup_no_follow("mnt").unwrap().is_root_of_mount());
    assert!(child.location().is_some());
    child.root_location().unmount().expect("unmount child");
    mounted
        .root_location()
        .unmount()
        .expect("unmount parent with fresh admission");
}
