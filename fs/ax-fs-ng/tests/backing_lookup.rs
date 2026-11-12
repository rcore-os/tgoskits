#![cfg(feature = "host-test")]

use std::sync::Arc;

use ax_fs_ng::MemoryFs;
use ax_runtime as _;
use axfs_ng_vfs::{
    Location, Mountpoint, MutationCredentials, MutationRetire, NamespaceAdmissions,
    NamespaceOperationScope, NamespaceQuery, NamespaceRead, NodePermission, NodeType,
    PreparedWrite, VfsError,
};

fn create(parent: &Location, name: &str, kind: NodeType) -> Location {
    parent
        .create(
            name,
            kind,
            NodePermission::from_bits_truncate(0o755),
            0,
            0,
            &MutationCredentials::root(),
        )
        .unwrap()
}

#[test]
fn backing_lookup_keeps_exact_mount_and_backend_contents_under_child_mounts() {
    let filesystem = MemoryFs::new();
    let root = Mountpoint::new_root(&filesystem).root_location();
    let covered = create(&root, "covered", NodeType::Directory);
    let underlying = create(&covered, "underlying", NodeType::RegularFile);
    let nested = create(&covered, "nested", NodeType::Directory);
    let leaf = create(&nested, "leaf", NodeType::RegularFile);
    let link = covered
        .create_symlink(
            "link",
            "underlying",
            NodePermission::from_bits_truncate(0o777),
            0,
            0,
            &MutationCredentials::root(),
        )
        .unwrap();

    let covering_fs = MemoryFs::new();
    let covering_root = Mountpoint::new_root(&covering_fs).root_location();
    create(&covering_root, "different", NodeType::RegularFile);
    let covering_mount = covered.mount(&covering_fs).unwrap();
    let nested_mount = nested.mount(&covering_fs).unwrap();

    let visible = root.lookup_no_follow("covered").unwrap();
    assert!(Arc::ptr_eq(visible.mountpoint(), &covering_mount));
    assert!(visible.lookup_no_follow("different").unwrap().is_file());
    assert!(matches!(
        visible.lookup_no_follow("underlying"),
        Err(VfsError::NotFound)
    ));

    let fixed = root.lookup_backing("covered").unwrap();
    assert!(
        Arc::ptr_eq(fixed.mountpoint(), root.mountpoint()),
        "backing lookup crossed the covering child mount"
    );
    assert!(fixed.entry().ptr_eq(covered.entry()));
    assert!(
        fixed
            .lookup_backing("underlying")
            .unwrap()
            .entry()
            .ptr_eq(underlying.entry())
    );
    assert!(matches!(
        fixed.lookup_backing("different"),
        Err(VfsError::NotFound)
    ));
    let fixed_nested = fixed.lookup_backing("nested").unwrap();
    assert!(Arc::ptr_eq(fixed_nested.mountpoint(), root.mountpoint()));
    assert!(fixed_nested.entry().ptr_eq(nested.entry()));
    assert!(
        fixed_nested
            .lookup_backing("leaf")
            .unwrap()
            .entry()
            .ptr_eq(leaf.entry())
    );
    assert!(Arc::ptr_eq(
        fixed.lookup_no_follow("nested").unwrap().mountpoint(),
        &nested_mount
    ));
    let fixed_link = fixed.lookup_backing("link").unwrap();
    assert!(fixed_link.entry().ptr_eq(link.entry()));
    assert_eq!(fixed_link.node_type(), NodeType::Symlink);
    assert_eq!(fixed_link.read_link().unwrap(), "underlying");
    assert!(matches!(
        fixed.lookup_backing("absent"),
        Err(VfsError::NotFound)
    ));
    assert!(matches!(
        fixed.lookup_backing("nested/leaf"),
        Err(VfsError::InvalidInput)
    ));
    assert!(matches!(
        underlying.lookup_backing("child"),
        Err(VfsError::NotADirectory)
    ));
    assert!(matches!(
        underlying.lookup_backing("bad/name"),
        Err(VfsError::InvalidInput)
    ));

    // Prepare the actual filesystem right before entering any backend gate.
    // No ordinary lookup or mutation is used while these admissions are live.
    let write = PreparedWrite::acquire(&root).unwrap();
    let retired = MutationRetire::new();
    let admissions = NamespaceAdmissions::acquire(&write).unwrap();
    let admission = admissions.for_location(&root).unwrap();
    let admitted = admission
        .lookup_backing(&root, "covered", &retired)
        .unwrap();
    let admitted_file = admission
        .lookup_backing(&admitted, "underlying", &retired)
        .unwrap();
    let admitted_nested = admission
        .lookup_backing(&admitted, "nested", &retired)
        .unwrap();
    let admitted_leaf = admission
        .lookup_backing(&admitted_nested, "leaf", &retired)
        .unwrap();
    let admitted_link = admission
        .lookup_backing(&admitted, "link", &retired)
        .unwrap();
    for (actual, expected) in [
        (&admitted, &covered),
        (&admitted_file, &underlying),
        (&admitted_nested, &nested),
        (&admitted_leaf, &leaf),
        (&admitted_link, &link),
    ] {
        assert!(Arc::ptr_eq(actual.mountpoint(), root.mountpoint()));
        // Native queries return fresh aliases, not the directory cache's Arc.
        assert!(core::ptr::addr_eq(
            actual.filesystem(),
            expected.filesystem()
        ));
        assert_eq!(actual.inode(), expected.inode());
        assert_eq!(actual.node_type(), expected.node_type());
    }
    assert!(matches!(
        admission.lookup_backing(&admitted, "different", &retired),
        Err(VfsError::NotFound)
    ));
    assert!(matches!(
        admission.lookup_backing(&admitted, "absent", &retired),
        Err(VfsError::NotFound)
    ));
    assert!(matches!(
        admission.lookup_backing(&admitted, "nested/leaf", &retired),
        Err(VfsError::InvalidInput)
    ));
    assert!(matches!(
        admission.lookup_backing(&admitted_file, "child", &retired),
        Err(VfsError::NotADirectory)
    ));
    assert!(matches!(
        admission.lookup_backing(&admitted_file, "bad/name", &retired),
        Err(VfsError::InvalidInput)
    ));
    drop(admissions);
    drop(retired);
    // Exercise the full-context first hop and its native default request path
    // against the same MemoryFs and already cached/covered backing entries.
    // Pure queries register no fake root or child completion.
    let scope = NamespaceOperationScope::<axfs_ng_vfs::NamespaceEffect>::acquire(&write).unwrap();
    {
        let context = scope.context();
        let selected = context.lookup_backing(&root, "covered").unwrap();
        let nested = context.lookup_backing(&selected, "nested").unwrap();
        let terminal = context.lookup_backing(&selected, "link").unwrap();
        for (actual, expected) in [
            (&selected, &covered),
            (&nested, &fixed_nested),
            (&terminal, &link),
        ] {
            assert!(Arc::ptr_eq(actual.mountpoint(), root.mountpoint()));
            assert_eq!(actual.inode(), expected.inode());
            assert_eq!(actual.node_type(), expected.node_type());
        }
        assert_eq!(terminal.node_type(), NodeType::Symlink);
        assert_eq!(
            context.lookup_backing(&nested, "leaf").unwrap().inode(),
            leaf.inode()
        );
        assert!(matches!(
            context.query(NamespaceQuery::Lookup {
                parent: selected.entry(),
                name: "absent"
            }),
            Ok(NamespaceRead::Entry(None))
        ));
        assert!(matches!(
            context.lookup_backing(&selected, "absent"),
            Err(VfsError::NotFound)
        ));
        assert!(matches!(
            context.lookup_backing(&selected, "different"),
            Err(VfsError::NotFound)
        ));
    }
    drop(scope);
    drop(write);
    assert_eq!(admitted_link.node_type(), NodeType::Symlink);
    assert_eq!(admitted_link.read_link().unwrap(), "underlying");
    nested_mount.detach().unwrap();
    covering_mount.detach().unwrap();
}
