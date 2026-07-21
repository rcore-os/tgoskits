use alloc::{format, string::String, vec::Vec};

use axtest::prelude::*;

#[axtest::def_test]
fn axfs_ng_vfs_path_rules_hold() {
    use axfs_ng_vfs::path::{Component, Path, PathBuf};

    let path = Path::new("/alpha/./beta//gamma/");
    let components: Vec<_> = path
        .components()
        .map(|component| component.as_str())
        .collect();
    ax_assert_eq!(components.as_slice(), ["/", "alpha", "beta", "gamma"]);
    ax_assert!(path.has_trailing_slash());
    ax_assert!(!Path::new("/").has_trailing_slash());
    ax_assert_eq!(Path::new("../a/b/c").file_name(), Some("c"));
    ax_assert_eq!(Path::new("a/..").file_name(), None);
    ax_assert_eq!(Path::new("/a/b").parent().map(Path::as_str), Some("/a/"));
    ax_assert!(Path::new("/a/b").is_absolute());
    ax_assert!(!Path::new("a/b").is_absolute());

    let normalized = Path::new("/a/./b/../c").normalize().unwrap();
    ax_assert_eq!(normalized.as_str(), "/a/c");
    ax_assert!(Path::new("a/../..").normalize().is_none());

    let mut path_buf = PathBuf::from("var");
    path_buf.push("log");
    ax_assert_eq!(path_buf.as_str(), "var/log");
    ax_assert!(path_buf.pop());
    ax_assert_eq!(path_buf.as_str(), "var/");

    let components: Vec<_> = Path::new("./fs").components().rev().collect();
    ax_assert_eq!(
        components.as_slice(),
        [Component::Normal("fs"), Component::CurDir]
    );
}

#[axtest::def_test]
fn axfs_ng_vfs_type_rules_hold() {
    use axfs_ng_vfs::{DeviceId, FsIoEvents, NodePermission, NodeType, Reference, TypeMap};

    ax_assert_eq!(NodeType::from(0o10), NodeType::RegularFile);
    ax_assert_eq!(NodeType::from(0o12), NodeType::Symlink);
    ax_assert_eq!(NodeType::from(0xff), NodeType::Unknown);
    ax_assert_eq!(NodePermission::default().bits(), 0o666);
    ax_assert!(
        (NodePermission::OWNER_READ | NodePermission::OWNER_WRITE)
            .contains(NodePermission::OWNER_WRITE)
    );

    let device = DeviceId::new(0x12345, 0x6789ab);
    ax_assert_eq!(device.major(), 0x12345);
    ax_assert_eq!(device.minor(), 0x6789ab);
    ax_assert!(format!("{device:?}").contains("major"));

    let events = FsIoEvents::IN | FsIoEvents::OUT;
    ax_assert!(events.contains(FsIoEvents::IN));
    ax_assert!(!events.contains(FsIoEvents::ERR));

    let mut type_map = TypeMap::new();
    ax_assert!(type_map.get::<u32>().is_none());
    type_map.insert(42_u32);
    ax_assert_eq!(*type_map.get::<u32>().unwrap(), 42);
    ax_assert_eq!(*type_map.get_or_insert_with(|| 7_u32), 42);
    ax_assert_eq!(Reference::root().key(), (0, String::new()));
}
