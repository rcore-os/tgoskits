use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    task::Context,
    time::Duration,
};

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
fn axfs_ng_vfs_path_ownership_and_join_rules_hold() {
    use alloc::sync::Arc;

    use axfs_ng_vfs::path::{Component, Path, PathBuf};

    let path = Path::new("alpha/beta");
    ax_assert_eq!(path.as_bytes(), b"alpha/beta");
    ax_assert_eq!(path.to_string(), "alpha/beta");
    ax_assert_eq!(path.parent().map(Path::as_str), Some("alpha/"));
    ax_assert_eq!(Path::new("/").parent(), None);
    ax_assert_eq!(Path::new(".").parent().map(Path::as_str), Some(""));
    ax_assert_eq!(Path::new("..").parent().map(Path::as_str), Some(""));

    let owned = path.to_owned();
    ax_assert_eq!(owned.as_str(), "alpha/beta");
    ax_assert_eq!(<PathBuf as AsRef<str>>::as_ref(&owned), "alpha/beta");
    ax_assert_eq!(owned.to_string(), "alpha/beta");
    ax_assert_eq!(format!("{owned:?}"), "PathBuf { inner: \"alpha/beta\" }");

    let arc_path: Arc<Path> = Arc::from(path);
    ax_assert_eq!(arc_path.as_str(), "alpha/beta");

    let collected: PathBuf = ["var", "log", "/tmp", "trace"].into_iter().collect();
    ax_assert_eq!(collected.as_str(), "/tmp/trace");

    let mut empty = PathBuf::new();
    empty.push("");
    ax_assert_eq!(empty.as_str(), "");
    empty.push("/root");
    ax_assert_eq!(empty.as_str(), "/root");
    empty.push("child");
    ax_assert_eq!(empty.as_str(), "/root/child");
    ax_assert!(empty.pop());
    ax_assert_eq!(empty.as_str(), "/root/");
    ax_assert!(empty.pop());
    ax_assert_eq!(empty.as_str(), "/");
    ax_assert!(!empty.pop());

    let joined = Path::new("/root").join("leaf");
    ax_assert_eq!(joined.as_str(), "/root/leaf");

    let components: Vec<_> = Path::new("../fds/").components().collect();
    let mut reversed: Vec<_> = Path::new("../fds/").components().rev().collect();
    reversed.reverse();
    ax_assert_eq!(components, reversed);
    ax_assert_eq!(
        components.as_slice(),
        [Component::ParentDir, Component::Normal("fds")]
    );
}

#[axtest::def_test]
fn axfs_ng_vfs_device_and_metadata_update_rules_hold() {
    use axfs_ng_vfs::{DeviceId, MetadataUpdate, NodePermission, NodeType};

    for (raw, node_type) in [
        (0o1, NodeType::Fifo),
        (0o2, NodeType::CharacterDevice),
        (0o4, NodeType::Directory),
        (0o6, NodeType::BlockDevice),
        (0o10, NodeType::RegularFile),
        (0o12, NodeType::Symlink),
        (0o14, NodeType::Socket),
        (0, NodeType::Unknown),
    ] {
        ax_assert_eq!(NodeType::from(raw), node_type);
    }

    let permission = NodePermission::SET_UID
        | NodePermission::SET_GID
        | NodePermission::STICKY
        | NodePermission::OWNER_EXEC
        | NodePermission::GROUP_EXEC
        | NodePermission::OTHER_EXEC;
    ax_assert!(permission.contains(NodePermission::SET_UID));
    ax_assert!(format!("{permission:?}").contains("OWNER_EXEC"));

    let device = DeviceId::new(0xffff_f123, 0xffff_fe45);
    ax_assert_eq!(device.major(), 0xffff_f123);
    ax_assert_eq!(device.minor(), 0xffff_fe45);
    ax_assert_eq!(
        format!("{device:?}"),
        "DeviceId { major: 4294963491, minor: 4294966853 }"
    );

    let update = MetadataUpdate {
        mode: Some(permission),
        owner: Some((1000, 1001)),
        rdev: Some(device),
        atime: Some(Duration::from_secs(10)),
        mtime: Some(Duration::from_secs(20)),
    };
    ax_assert!(update.mode.unwrap().contains(NodePermission::STICKY));
    ax_assert_eq!(update.owner, Some((1000, 1001)));
    ax_assert_eq!(update.rdev.unwrap().minor(), 0xffff_fe45);
    ax_assert_eq!(update.atime.unwrap().as_secs(), 10);
    ax_assert_eq!(update.mtime.unwrap().as_secs(), 20);
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

#[axtest::def_test]
fn axfs_ng_vfs_file_node_defaults_hold() {
    use ax_errno::AxError;
    use axfs_ng_vfs::{
        DeviceId, DirEntry, FileNode, FileNodeOps, Filesystem, FilesystemOps, FsIoEvents,
        FsPollable, Metadata, MetadataUpdate, NodeFlags, NodeOps, NodePermission, NodeType,
        Reference, StatFs, VfsResult,
    };

    #[derive(Debug)]
    struct TestFilesystem;

    impl FilesystemOps for TestFilesystem {
        fn name(&self) -> &str {
            "coveragefs"
        }

        fn root_dir(&self) -> DirEntry {
            panic!("root_dir is not needed by this coverage test")
        }

        fn stat(&self) -> VfsResult<StatFs> {
            Ok(StatFs {
                fs_type: 0xCAFE,
                block_size: 4096,
                blocks: 16,
                blocks_free: 8,
                blocks_available: 7,
                file_count: 4,
                free_file_count: 3,
                name_length: 255,
                fragment_size: 4096,
                mount_flags: 0,
            })
        }
    }

    #[derive(Debug)]
    struct TestFile {
        inode: u64,
        data: &'static [u8],
    }

    impl NodeOps for TestFile {
        fn inode(&self) -> u64 {
            self.inode
        }

        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(Metadata {
                device: 1,
                inode: self.inode,
                nlink: 1,
                mode: NodePermission::default(),
                node_type: NodeType::RegularFile,
                uid: 1000,
                gid: 1000,
                size: self.data.len() as u64,
                block_size: 4096,
                blocks: 1,
                rdev: DeviceId::default(),
                atime: Duration::from_secs(1),
                mtime: Duration::from_secs(2),
                ctime: Duration::from_secs(3),
            })
        }

        fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
            assert!(update.mode.is_none());
            Ok(())
        }

        fn filesystem(&self) -> &dyn FilesystemOps {
            &TestFilesystem
        }

        fn sync(&self, data_only: bool) -> VfsResult<()> {
            assert!(!data_only);
            Ok(())
        }

        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }

        fn flags(&self) -> NodeFlags {
            NodeFlags::NON_CACHEABLE | NodeFlags::BLOCKING
        }
    }

    impl FsPollable for TestFile {
        fn poll(&self) -> FsIoEvents {
            FsIoEvents::IN | FsIoEvents::OUT
        }

        fn register(&self, _context: &mut Context<'_>, events: FsIoEvents) {
            assert!(events.contains(FsIoEvents::IN));
        }
    }

    impl FileNodeOps for TestFile {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
            let start = offset as usize;
            if start >= self.data.len() {
                return Ok(0);
            }
            let readable = self.data.len() - start;
            let copied = readable.min(buf.len());
            buf[..copied].copy_from_slice(&self.data[start..start + copied]);
            Ok(copied)
        }

        fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
            Err(AxError::ReadOnlyFilesystem)
        }

        fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
            Err(AxError::ReadOnlyFilesystem)
        }

        fn set_len(&self, _len: u64) -> VfsResult<()> {
            Err(AxError::ReadOnlyFilesystem)
        }

        fn set_symlink(&self, _target: &str) -> VfsResult<()> {
            Err(AxError::Unsupported)
        }
    }

    let ops = Arc::new(TestFile {
        inode: 99,
        data: b"target-path",
    });
    let file_node = Arc::new(FileNode::new(ops.clone()));
    ax_assert_eq!(file_node.inode(), 99);
    ax_assert_eq!(file_node.len().unwrap(), 11);
    ax_assert!(file_node.flags().contains(NodeFlags::NON_CACHEABLE));
    ax_assert_eq!(file_node.ioctl(0, 0), Err(AxError::NotATty));
    ax_assert_eq!(file_node.poll(), FsIoEvents::IN | FsIoEvents::OUT);
    ax_assert!(Arc::ptr_eq(
        &file_node.downcast::<TestFile>().unwrap(),
        &ops
    ));

    let entry = DirEntry::new_file(
        (*file_node).clone(),
        NodeType::Symlink,
        Reference::new(None, "link".into()),
    );
    ax_assert!(entry.is_file());
    ax_assert!(!entry.is_dir());
    ax_assert_eq!(entry.node_type(), NodeType::Symlink);
    ax_assert_eq!(entry.name(), "link");
    ax_assert_eq!(entry.key().1, "link");
    ax_assert_eq!(entry.metadata().unwrap().node_type, NodeType::Symlink);
    ax_assert_eq!(entry.read_link().unwrap(), "target-path");
    ax_assert_eq!(entry.ioctl(0, 0), Err(AxError::NotATty));
    ax_assert!(matches!(entry.as_dir(), Err(AxError::NotADirectory)));
    ax_assert!(entry.as_file().is_ok());
    ax_assert!(entry.downcast::<TestFile>().is_ok());
    ax_assert!(entry.is_root_of_mount());
    ax_assert_eq!(entry.absolute_path().unwrap().as_str(), "/link");
    ax_assert!(entry.downgrade().upgrade().unwrap().ptr_eq(&entry));

    let mut user_data = entry.user_data();
    user_data.insert(vec![1_u8, 2, 3]);
    ax_assert_eq!(user_data.get::<Vec<u8>>().unwrap().as_slice(), [1, 2, 3]);

    let fs_ops = Arc::new(TestFilesystem);
    let filesystem = Filesystem::new(fs_ops.clone());
    ax_assert_eq!(filesystem.name(), "coveragefs");
    ax_assert!(!filesystem.is_readonly());
    ax_assert_eq!(filesystem.stat().unwrap().fs_type, 0xCAFE);
    ax_assert_eq!(fs_ops.flush(), Ok(()));
}

#[axtest::def_test]
fn axfs_ng_vfs_dir_node_cache_and_mutation_rules_hold() {
    use ax_errno::AxError;
    use axfs_ng_vfs::{
        DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps,
        FilesystemOps, FsIoEvents, FsPollable, Metadata, MetadataUpdate, Mutex, NodeFlags, NodeOps,
        NodePermission, NodeType, OpenOptions, Reference, VfsResult, WeakDirEntry,
    };

    #[derive(Debug)]
    struct DirTestFilesystem;

    impl FilesystemOps for DirTestFilesystem {
        fn name(&self) -> &str {
            "dir-coveragefs"
        }

        fn root_dir(&self) -> DirEntry {
            panic!("root_dir is not needed by this coverage test")
        }

        fn stat(&self) -> VfsResult<axfs_ng_vfs::StatFs> {
            Err(AxError::Unsupported)
        }
    }

    #[derive(Debug)]
    struct DirTestFile {
        inode: u64,
    }

    impl NodeOps for DirTestFile {
        fn inode(&self) -> u64 {
            self.inode
        }

        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(Metadata {
                device: 2,
                inode: self.inode,
                nlink: 1,
                mode: NodePermission::default(),
                node_type: NodeType::RegularFile,
                uid: 0,
                gid: 0,
                size: 0,
                block_size: 4096,
                blocks: 0,
                rdev: DeviceId::default(),
                atime: Duration::ZERO,
                mtime: Duration::ZERO,
                ctime: Duration::ZERO,
            })
        }

        fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
            Ok(())
        }

        fn filesystem(&self) -> &dyn FilesystemOps {
            &DirTestFilesystem
        }

        fn sync(&self, _data_only: bool) -> VfsResult<()> {
            Ok(())
        }

        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    impl FsPollable for DirTestFile {
        fn poll(&self) -> FsIoEvents {
            FsIoEvents::IN | FsIoEvents::OUT
        }

        fn register(&self, _context: &mut Context<'_>, _events: FsIoEvents) {}
    }

    impl FileNodeOps for DirTestFile {
        fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
            Ok(0)
        }

        fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
            Err(AxError::ReadOnlyFilesystem)
        }

        fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
            Err(AxError::ReadOnlyFilesystem)
        }

        fn set_len(&self, _len: u64) -> VfsResult<()> {
            Err(AxError::ReadOnlyFilesystem)
        }

        fn set_symlink(&self, _target: &str) -> VfsResult<()> {
            Err(AxError::Unsupported)
        }
    }

    struct DirTestDir {
        inode: u64,
        self_ref: WeakDirEntry,
        children: Mutex<Vec<(String, DirEntry)>>,
        next_inode: AtomicU64,
        lookup_count: AtomicUsize,
    }

    impl DirTestDir {
        fn parent(&self) -> Option<DirEntry> {
            self.self_ref.upgrade()
        }

        fn make_file_entry(&self, name: &str, node_type: NodeType) -> DirEntry {
            let inode = self.next_inode.fetch_add(1, Ordering::AcqRel);
            let file = FileNode::new(Arc::new(DirTestFile { inode }));
            DirEntry::new_file(file, node_type, Reference::new(self.parent(), name.into()))
        }
    }

    impl NodeOps for DirTestDir {
        fn inode(&self) -> u64 {
            self.inode
        }

        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(Metadata {
                device: 2,
                inode: self.inode,
                nlink: 2,
                mode: NodePermission::default(),
                node_type: NodeType::Directory,
                uid: 0,
                gid: 0,
                size: 0,
                block_size: 4096,
                blocks: 0,
                rdev: DeviceId::default(),
                atime: Duration::ZERO,
                mtime: Duration::ZERO,
                ctime: Duration::ZERO,
            })
        }

        fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
            Ok(())
        }

        fn filesystem(&self) -> &dyn FilesystemOps {
            &DirTestFilesystem
        }

        fn sync(&self, _data_only: bool) -> VfsResult<()> {
            Ok(())
        }

        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }

        fn flags(&self) -> NodeFlags {
            NodeFlags::ALWAYS_CACHE
        }
    }

    impl DirNodeOps for DirTestDir {
        fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
            let mut emitted = 0;
            let entries = self.children.lock();
            let mut all_entries = Vec::new();
            all_entries.push((".".to_string(), self.inode, NodeType::Directory));
            all_entries.push(("..".to_string(), self.inode, NodeType::Directory));
            for (name, entry) in entries.iter() {
                all_entries.push((name.clone(), entry.inode(), entry.node_type()));
            }
            for (index, (name, inode, node_type)) in all_entries.into_iter().enumerate() {
                if index < offset as usize {
                    continue;
                }
                if !sink.accept(&name, inode, node_type, index as u64 + 1) {
                    break;
                }
                emitted += 1;
            }
            Ok(emitted)
        }

        fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
            self.lookup_count.fetch_add(1, Ordering::AcqRel);
            self.children
                .lock()
                .iter()
                .find_map(|(child_name, entry)| (child_name == name).then(|| entry.clone()))
                .ok_or(AxError::NotFound)
        }

        fn create(
            &self,
            name: &str,
            node_type: NodeType,
            _permission: NodePermission,
            _uid: u32,
            _gid: u32,
        ) -> VfsResult<DirEntry> {
            if self.lookup(name).is_ok() {
                return Err(AxError::AlreadyExists);
            }
            let entry = self.make_file_entry(name, node_type);
            self.children.lock().push((name.into(), entry.clone()));
            Ok(entry)
        }

        fn link(&self, name: &str, node: &DirEntry) -> VfsResult<DirEntry> {
            if self.lookup(name).is_ok() {
                return Err(AxError::AlreadyExists);
            }
            let file = node.as_file()?.clone();
            let entry = DirEntry::new_file(
                file,
                node.node_type(),
                Reference::new(self.parent(), name.into()),
            );
            self.children.lock().push((name.into(), entry.clone()));
            Ok(entry)
        }

        fn unlink(&self, name: &str, _is_dir: bool) -> VfsResult<()> {
            let mut children = self.children.lock();
            let Some(index) = children
                .iter()
                .position(|(child_name, _)| child_name == name)
            else {
                return Err(AxError::NotFound);
            };
            children.remove(index);
            Ok(())
        }

        fn rename(&self, _src_name: &str, _dst_dir: &DirNode, _dst_name: &str) -> VfsResult<()> {
            Err(AxError::Unsupported)
        }
    }

    let root = DirEntry::new_dir(
        |weak| {
            DirNode::new(Arc::new(DirTestDir {
                inode: 10,
                self_ref: weak,
                children: Mutex::new(Vec::new()),
                next_inode: AtomicU64::new(100),
                lookup_count: AtomicUsize::new(0),
            }))
        },
        Reference::root(),
    );
    let dir = root.as_dir().unwrap();
    let ops = dir.downcast::<DirTestDir>().unwrap();

    ax_assert!(root.is_dir());
    ax_assert!(root.is_root_of_mount());
    ax_assert!(root.flags().contains(NodeFlags::ALWAYS_CACHE));
    ax_assert!(!dir.has_children().unwrap());
    ax_assert!(matches!(dir.lookup("."), Err(AxError::InvalidInput)));
    ax_assert!(matches!(dir.lookup("bad/name"), Err(AxError::InvalidInput)));
    ax_assert!(matches!(
        dir.lookup("bad\0name"),
        Err(AxError::InvalidInput)
    ));
    ax_assert!(matches!(
        dir.lookup(&"x".repeat(axfs_ng_vfs::path::MAX_NAME_LEN + 1)),
        Err(AxError::NameTooLong)
    ));

    let child = dir
        .create(
            "child",
            NodeType::RegularFile,
            NodePermission::default(),
            1,
            2,
        )
        .unwrap();
    ax_assert!(dir.has_children().unwrap());
    ax_assert_eq!(child.parent().unwrap().as_ptr(), root.as_ptr());
    ax_assert!(root.is_ancestor_of(&child).unwrap());
    ax_assert!(!child.is_ancestor_of(&root).unwrap());
    ax_assert_eq!(child.absolute_path().unwrap().as_str(), "/child");
    ax_assert!(dir.lookup_cache("child").is_some());

    let lookup_count = ops.lookup_count.load(Ordering::Acquire);
    ax_assert!(dir.lookup("child").unwrap().ptr_eq(&child));
    ax_assert_eq!(ops.lookup_count.load(Ordering::Acquire), lookup_count);

    {
        let mut child_data = child.user_data();
        child_data.insert(123_u32);
    }
    let hard_link = dir.link("hard", &child).unwrap();
    ax_assert_eq!(*hard_link.user_data().get::<u32>().unwrap(), 123);
    ax_assert!(dir.lookup_cache("hard").is_some());

    let options = OpenOptions {
        create: true,
        user: Some((11, 12)),
        ..Default::default()
    };
    let opened = dir.open_file("created-by-open", &options).unwrap();
    ax_assert_eq!(opened.name(), "created-by-open");
    let create_new_existing = OpenOptions {
        create_new: true,
        ..Default::default()
    };
    ax_assert!(matches!(
        dir.open_file("created-by-open", &create_new_existing),
        Err(AxError::AlreadyExists)
    ));

    ax_assert_eq!(dir.unlink("child", true), Err(AxError::NotADirectory));
    dir.unlink("child", false).unwrap();
    ax_assert!(dir.lookup_cache("child").is_none());
    ax_assert!(matches!(dir.lookup("child"), Err(AxError::NotFound)));
}

#[axtest::def_test]
fn axfs_ng_vfs_mount_tree_rules_hold() {
    use ax_errno::AxError;
    use axfs_ng_vfs::{
        DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FileNodeOps, Filesystem,
        FilesystemOps, FsIoEvents, FsPollable, Metadata, MetadataUpdate, Mountpoint, Mutex,
        NodeOps, NodePermission, NodeType, Reference, StatFs, VfsResult,
    };

    #[derive(Debug)]
    struct MountNodeFilesystem;

    static MOUNT_NODE_FILESYSTEM: MountNodeFilesystem = MountNodeFilesystem;

    impl FilesystemOps for MountNodeFilesystem {
        fn name(&self) -> &str {
            "mount-nodefs"
        }

        fn root_dir(&self) -> DirEntry {
            panic!("root_dir is not used through node filesystem references")
        }

        fn stat(&self) -> VfsResult<StatFs> {
            Err(AxError::Unsupported)
        }
    }

    #[derive(Debug)]
    struct MountTestFs {
        name: &'static str,
        root: DirEntry,
        readonly: bool,
    }

    impl FilesystemOps for MountTestFs {
        fn name(&self) -> &str {
            self.name
        }

        fn is_readonly(&self) -> bool {
            self.readonly
        }

        fn root_dir(&self) -> DirEntry {
            self.root.clone()
        }

        fn stat(&self) -> VfsResult<StatFs> {
            Ok(StatFs {
                fs_type: 0xABCD,
                block_size: 4096,
                blocks: 32,
                blocks_free: 16,
                blocks_available: 15,
                file_count: 8,
                free_file_count: 4,
                name_length: 255,
                fragment_size: 4096,
                mount_flags: 0,
            })
        }
    }

    #[derive(Debug)]
    struct MountTestFile {
        inode: u64,
    }

    impl NodeOps for MountTestFile {
        fn inode(&self) -> u64 {
            self.inode
        }

        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(test_metadata(self.inode, NodeType::RegularFile))
        }

        fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
            Ok(())
        }

        fn filesystem(&self) -> &dyn FilesystemOps {
            &MOUNT_NODE_FILESYSTEM
        }

        fn sync(&self, _data_only: bool) -> VfsResult<()> {
            Ok(())
        }

        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    impl FsPollable for MountTestFile {
        fn poll(&self) -> FsIoEvents {
            FsIoEvents::IN
        }

        fn register(&self, _context: &mut Context<'_>, _events: FsIoEvents) {}
    }

    impl FileNodeOps for MountTestFile {
        fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
            Ok(0)
        }

        fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
            Err(AxError::ReadOnlyFilesystem)
        }

        fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
            Err(AxError::ReadOnlyFilesystem)
        }

        fn set_len(&self, _len: u64) -> VfsResult<()> {
            Err(AxError::ReadOnlyFilesystem)
        }

        fn set_symlink(&self, _target: &str) -> VfsResult<()> {
            Err(AxError::Unsupported)
        }
    }

    struct MountTestDir {
        inode: u64,
        self_ref: axfs_ng_vfs::WeakDirEntry,
        children: Mutex<Vec<(String, DirEntry)>>,
        next_inode: AtomicU64,
    }

    impl MountTestDir {
        fn parent(&self) -> Option<DirEntry> {
            self.self_ref.upgrade()
        }

        fn make_entry(&self, name: &str, node_type: NodeType) -> DirEntry {
            let inode = self.next_inode.fetch_add(1, Ordering::AcqRel);
            match node_type {
                NodeType::Directory => DirEntry::new_dir(
                    |weak| {
                        DirNode::new(Arc::new(MountTestDir {
                            inode,
                            self_ref: weak,
                            children: Mutex::new(Vec::new()),
                            next_inode: AtomicU64::new(inode * 10),
                        }))
                    },
                    Reference::new(self.parent(), name.into()),
                ),
                _ => {
                    let file = FileNode::new(Arc::new(MountTestFile { inode }));
                    DirEntry::new_file(file, node_type, Reference::new(self.parent(), name.into()))
                }
            }
        }
    }

    impl NodeOps for MountTestDir {
        fn inode(&self) -> u64 {
            self.inode
        }

        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(test_metadata(self.inode, NodeType::Directory))
        }

        fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
            Ok(())
        }

        fn filesystem(&self) -> &dyn FilesystemOps {
            &MOUNT_NODE_FILESYSTEM
        }

        fn sync(&self, _data_only: bool) -> VfsResult<()> {
            Ok(())
        }

        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    impl DirNodeOps for MountTestDir {
        fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
            let children = self.children.lock();
            let mut entries = Vec::new();
            entries.push((".".to_string(), self.inode, NodeType::Directory));
            entries.push(("..".to_string(), self.inode, NodeType::Directory));
            for (name, entry) in children.iter() {
                entries.push((name.clone(), entry.inode(), entry.node_type()));
            }

            let mut emitted = 0;
            for (index, (name, inode, node_type)) in entries.into_iter().enumerate() {
                if index < offset as usize {
                    continue;
                }
                if !sink.accept(&name, inode, node_type, index as u64 + 1) {
                    break;
                }
                emitted += 1;
            }
            Ok(emitted)
        }

        fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
            self.children
                .lock()
                .iter()
                .find_map(|(child_name, entry)| (child_name == name).then(|| entry.clone()))
                .ok_or(AxError::NotFound)
        }

        fn create(
            &self,
            name: &str,
            node_type: NodeType,
            _permission: NodePermission,
            _uid: u32,
            _gid: u32,
        ) -> VfsResult<DirEntry> {
            if self.lookup(name).is_ok() {
                return Err(AxError::AlreadyExists);
            }
            let entry = self.make_entry(name, node_type);
            self.children.lock().push((name.into(), entry.clone()));
            Ok(entry)
        }

        fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
            Err(AxError::Unsupported)
        }

        fn unlink(&self, name: &str, _is_dir: bool) -> VfsResult<()> {
            let mut children = self.children.lock();
            let Some(index) = children
                .iter()
                .position(|(child_name, _)| child_name == name)
            else {
                return Err(AxError::NotFound);
            };
            children.remove(index);
            Ok(())
        }

        fn rename(&self, _src_name: &str, _dst_dir: &DirNode, _dst_name: &str) -> VfsResult<()> {
            Err(AxError::Unsupported)
        }
    }

    fn test_metadata(inode: u64, node_type: NodeType) -> Metadata {
        Metadata {
            device: 0,
            inode,
            nlink: 1,
            mode: NodePermission::default(),
            node_type,
            uid: 0,
            gid: 0,
            size: 0,
            block_size: 4096,
            blocks: 0,
            rdev: DeviceId::default(),
            atime: Duration::ZERO,
            mtime: Duration::ZERO,
            ctime: Duration::ZERO,
        }
    }

    fn new_root_entry(inode: u64, child_dirs: &[&str]) -> DirEntry {
        let root = DirEntry::new_dir(
            |weak| {
                DirNode::new(Arc::new(MountTestDir {
                    inode,
                    self_ref: weak,
                    children: Mutex::new(Vec::new()),
                    next_inode: AtomicU64::new(inode * 10),
                }))
            },
            Reference::root(),
        );
        let root_dir = root.as_dir().unwrap();
        for name in child_dirs {
            root_dir
                .create(name, NodeType::Directory, NodePermission::default(), 0, 0)
                .unwrap();
        }
        root
    }

    fn new_fs(name: &'static str, readonly: bool, inode: u64, child_dirs: &[&str]) -> Filesystem {
        Filesystem::new(Arc::new(MountTestFs {
            name,
            root: new_root_entry(inode, child_dirs),
            readonly,
        }))
    }

    let root_fs = new_fs(
        "rootfs",
        false,
        10,
        &["mnt", "bind", "move-target", "put-old"],
    );
    let root_mount = Mountpoint::new_root(&root_fs);
    let root = root_mount.root_location();
    ax_assert!(root.is_root());
    ax_assert!(root_mount.is_root());
    ax_assert_eq!(root.metadata().unwrap().device, root_mount.device());
    ax_assert!(!root_mount.mark_expired());
    ax_assert!(root_mount.mark_expired());
    root_mount.clear_expired();
    ax_assert!(!root_mount.mark_expired());

    root.create(
        "created",
        NodeType::RegularFile,
        NodePermission::default(),
        0,
        0,
    )
    .unwrap();
    ax_assert!(root.lookup_no_follow("created").unwrap().is_file());
    ax_assert!(matches!(
        root.create_transient_mount_dir("transient", NodePermission::default(), 1, 2),
        Err(AxError::InvalidInput)
    ));

    root_mount.set_readonly(true);
    ax_assert!(matches!(
        root.create(
            "blocked",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        ),
        Err(AxError::ReadOnlyFilesystem)
    ));
    let transient = root
        .create_transient_mount_dir("transient", NodePermission::default(), 1, 2)
        .unwrap();
    ax_assert!(transient.is_dir());
    ax_assert_eq!(transient.name(), "transient");
    ax_assert_eq!(
        root.lookup_no_follow("transient").unwrap().inode(),
        transient.inode()
    );
    root_mount.set_readonly(false);

    let child_fs = new_fs("childfs", false, 20, &["put-old", "inside"]);
    let mnt = root.lookup_no_follow("mnt").unwrap();
    let child_mount = mnt.mount(&child_fs).unwrap();
    ax_assert!(mnt.is_mountpoint());
    ax_assert_eq!(root_mount.children().len(), 1);
    ax_assert!(matches!(mnt.mount(&child_fs), Err(AxError::ResourceBusy)));

    let mounted_root = root.lookup_no_follow("mnt").unwrap();
    ax_assert!(mounted_root.is_root_of_mount());
    ax_assert_eq!(mounted_root.name(), "mnt");
    ax_assert_eq!(
        mounted_root.metadata().unwrap().device,
        child_mount.device()
    );
    ax_assert_eq!(mounted_root.absolute_path().unwrap().as_str(), "/mnt");

    let bind_target = root.lookup_no_follow("bind").unwrap();
    let bind_mount = bind_target
        .bind_mount(&child_mount.root_location(), true)
        .unwrap();
    ax_assert_eq!(bind_mount.device(), child_mount.device());
    ax_assert!(matches!(
        bind_target.bind_mount(&child_mount.root_location(), false),
        Err(AxError::ResourceBusy)
    ));
    ax_assert!(root.lookup_no_follow("bind").unwrap().is_root_of_mount());

    let bind_root = root.lookup_no_follow("bind").unwrap();
    let move_target = root.lookup_no_follow("move-target").unwrap();
    bind_root.move_mount(&move_target).unwrap();
    ax_assert!(!bind_target.is_mountpoint());
    let moved_root = root.lookup_no_follow("move-target").unwrap();
    ax_assert!(moved_root.is_root_of_mount());
    ax_assert_eq!(moved_root.name(), "move-target");
    moved_root.detach_mount().unwrap();
    ax_assert!(!move_target.is_mountpoint());

    let cloned_root = root_mount.clone_tree();
    ax_assert!(cloned_root.is_root());
    ax_assert_eq!(cloned_root.children().len(), root_mount.children().len());

    let put_old = mounted_root.lookup_no_follow("put-old").unwrap();
    root_mount
        .pivot_mount(mounted_root.mountpoint(), &put_old)
        .unwrap();
    ax_assert!(mounted_root.mountpoint().is_root());
    ax_assert!(!root_mount.is_root());
    ax_assert_eq!(root_mount.location().unwrap().name(), "put-old");

    root_mount.root_location().unmount_all().unwrap();
}

#[derive(Debug)]
struct MoreNodeFilesystem;

static MORE_NODE_FILESYSTEM: MoreNodeFilesystem = MoreNodeFilesystem;

impl axfs_ng_vfs::FilesystemOps for MoreNodeFilesystem {
    fn name(&self) -> &str {
        "more-nodefs"
    }

    fn root_dir(&self) -> axfs_ng_vfs::DirEntry {
        panic!("root_dir is not used through standalone node references")
    }

    fn stat(&self) -> axfs_ng_vfs::VfsResult<axfs_ng_vfs::StatFs> {
        Err(ax_errno::AxError::Unsupported)
    }
}

#[derive(Debug)]
struct MoreTestFs {
    name: &'static str,
    root: axfs_ng_vfs::DirEntry,
    readonly: bool,
}

impl axfs_ng_vfs::FilesystemOps for MoreTestFs {
    fn name(&self) -> &str {
        self.name
    }

    fn is_readonly(&self) -> bool {
        self.readonly
    }

    fn root_dir(&self) -> axfs_ng_vfs::DirEntry {
        self.root.clone()
    }

    fn stat(&self) -> axfs_ng_vfs::VfsResult<axfs_ng_vfs::StatFs> {
        Ok(axfs_ng_vfs::StatFs {
            fs_type: 0xF500,
            block_size: 4096,
            blocks: 64,
            blocks_free: 32,
            blocks_available: 31,
            file_count: 16,
            free_file_count: 8,
            name_length: 255,
            fragment_size: 4096,
            mount_flags: 0,
        })
    }
}

#[derive(Debug)]
struct MoreTestFile {
    inode: u64,
    symlink: Option<&'static str>,
}

impl axfs_ng_vfs::NodeOps for MoreTestFile {
    fn inode(&self) -> u64 {
        self.inode
    }

    fn metadata(&self) -> axfs_ng_vfs::VfsResult<axfs_ng_vfs::Metadata> {
        Ok(more_metadata(
            self.inode,
            if self.symlink.is_some() {
                axfs_ng_vfs::NodeType::Symlink
            } else {
                axfs_ng_vfs::NodeType::RegularFile
            },
        ))
    }

    fn update_metadata(&self, _update: axfs_ng_vfs::MetadataUpdate) -> axfs_ng_vfs::VfsResult<()> {
        Ok(())
    }

    fn filesystem(&self) -> &dyn axfs_ng_vfs::FilesystemOps {
        &MORE_NODE_FILESYSTEM
    }

    fn sync(&self, _data_only: bool) -> axfs_ng_vfs::VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl axfs_ng_vfs::FsPollable for MoreTestFile {
    fn poll(&self) -> axfs_ng_vfs::FsIoEvents {
        axfs_ng_vfs::FsIoEvents::IN | axfs_ng_vfs::FsIoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: axfs_ng_vfs::FsIoEvents) {}
}

impl axfs_ng_vfs::FileNodeOps for MoreTestFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> axfs_ng_vfs::VfsResult<usize> {
        let Some(target) = self.symlink else {
            return Ok(0);
        };
        let start = offset as usize;
        if start >= target.len() {
            return Ok(0);
        }
        let bytes = target.as_bytes();
        let copied = buf.len().min(bytes.len() - start);
        buf[..copied].copy_from_slice(&bytes[start..start + copied]);
        Ok(copied)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> axfs_ng_vfs::VfsResult<usize> {
        Err(ax_errno::AxError::ReadOnlyFilesystem)
    }

    fn append(&self, _buf: &[u8]) -> axfs_ng_vfs::VfsResult<(usize, u64)> {
        Err(ax_errno::AxError::ReadOnlyFilesystem)
    }

    fn set_len(&self, _len: u64) -> axfs_ng_vfs::VfsResult<()> {
        Err(ax_errno::AxError::ReadOnlyFilesystem)
    }

    fn set_symlink(&self, _target: &str) -> axfs_ng_vfs::VfsResult<()> {
        Err(ax_errno::AxError::Unsupported)
    }
}

struct MoreTestDir {
    inode: u64,
    self_ref: axfs_ng_vfs::WeakDirEntry,
    children: axfs_ng_vfs::Mutex<Vec<(String, axfs_ng_vfs::DirEntry)>>,
    next_inode: AtomicU64,
}

impl MoreTestDir {
    fn parent(&self) -> Option<axfs_ng_vfs::DirEntry> {
        self.self_ref.upgrade()
    }

    fn make_entry(&self, name: &str, node_type: axfs_ng_vfs::NodeType) -> axfs_ng_vfs::DirEntry {
        let inode = self.next_inode.fetch_add(1, Ordering::AcqRel);
        match node_type {
            axfs_ng_vfs::NodeType::Directory => axfs_ng_vfs::DirEntry::new_dir(
                |weak| {
                    axfs_ng_vfs::DirNode::new(Arc::new(MoreTestDir {
                        inode,
                        self_ref: weak,
                        children: axfs_ng_vfs::Mutex::new(Vec::new()),
                        next_inode: AtomicU64::new(inode * 10),
                    }))
                },
                axfs_ng_vfs::Reference::new(self.parent(), name.into()),
            ),
            axfs_ng_vfs::NodeType::Symlink => {
                let file = axfs_ng_vfs::FileNode::new(Arc::new(MoreTestFile {
                    inode,
                    symlink: Some("/target"),
                }));
                axfs_ng_vfs::DirEntry::new_file(
                    file,
                    node_type,
                    axfs_ng_vfs::Reference::new(self.parent(), name.into()),
                )
            }
            _ => {
                let file = axfs_ng_vfs::FileNode::new(Arc::new(MoreTestFile {
                    inode,
                    symlink: None,
                }));
                axfs_ng_vfs::DirEntry::new_file(
                    file,
                    node_type,
                    axfs_ng_vfs::Reference::new(self.parent(), name.into()),
                )
            }
        }
    }

    fn remove_child(&self, name: &str) -> axfs_ng_vfs::VfsResult<axfs_ng_vfs::DirEntry> {
        let mut children = self.children.lock();
        let Some(index) = children
            .iter()
            .position(|(child_name, _)| child_name == name)
        else {
            return Err(ax_errno::AxError::NotFound);
        };
        Ok(children.remove(index).1)
    }
}

impl axfs_ng_vfs::NodeOps for MoreTestDir {
    fn inode(&self) -> u64 {
        self.inode
    }

    fn metadata(&self) -> axfs_ng_vfs::VfsResult<axfs_ng_vfs::Metadata> {
        Ok(more_metadata(self.inode, axfs_ng_vfs::NodeType::Directory))
    }

    fn update_metadata(&self, _update: axfs_ng_vfs::MetadataUpdate) -> axfs_ng_vfs::VfsResult<()> {
        Ok(())
    }

    fn filesystem(&self) -> &dyn axfs_ng_vfs::FilesystemOps {
        &MORE_NODE_FILESYSTEM
    }

    fn sync(&self, _data_only: bool) -> axfs_ng_vfs::VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl axfs_ng_vfs::DirNodeOps for MoreTestDir {
    fn read_dir(
        &self,
        offset: u64,
        sink: &mut dyn axfs_ng_vfs::DirEntrySink,
    ) -> axfs_ng_vfs::VfsResult<usize> {
        let children = self.children.lock();
        let mut entries = Vec::new();
        entries.push((".".into(), self.inode, axfs_ng_vfs::NodeType::Directory));
        entries.push(("..".into(), self.inode, axfs_ng_vfs::NodeType::Directory));
        for (name, entry) in children.iter() {
            entries.push((name.clone(), entry.inode(), entry.node_type()));
        }

        let mut emitted = 0;
        for (index, (name, inode, node_type)) in entries.into_iter().enumerate() {
            if index < offset as usize {
                continue;
            }
            if !sink.accept(&name, inode, node_type, index as u64 + 1) {
                break;
            }
            emitted += 1;
        }
        Ok(emitted)
    }

    fn lookup(&self, name: &str) -> axfs_ng_vfs::VfsResult<axfs_ng_vfs::DirEntry> {
        self.children
            .lock()
            .iter()
            .find_map(|(child_name, entry)| (child_name == name).then(|| entry.clone()))
            .ok_or(ax_errno::AxError::NotFound)
    }

    fn create(
        &self,
        name: &str,
        node_type: axfs_ng_vfs::NodeType,
        _permission: axfs_ng_vfs::NodePermission,
        _uid: u32,
        _gid: u32,
    ) -> axfs_ng_vfs::VfsResult<axfs_ng_vfs::DirEntry> {
        if self.lookup(name).is_ok() {
            return Err(ax_errno::AxError::AlreadyExists);
        }
        let entry = self.make_entry(name, node_type);
        self.children.lock().push((name.into(), entry.clone()));
        Ok(entry)
    }

    fn link(
        &self,
        name: &str,
        node: &axfs_ng_vfs::DirEntry,
    ) -> axfs_ng_vfs::VfsResult<axfs_ng_vfs::DirEntry> {
        if self.lookup(name).is_ok() {
            return Err(ax_errno::AxError::AlreadyExists);
        }
        let file = node.as_file()?.clone();
        let entry = axfs_ng_vfs::DirEntry::new_file(
            file,
            node.node_type(),
            axfs_ng_vfs::Reference::new(self.parent(), name.into()),
        );
        self.children.lock().push((name.into(), entry.clone()));
        Ok(entry)
    }

    fn unlink(&self, name: &str, _is_dir: bool) -> axfs_ng_vfs::VfsResult<()> {
        self.remove_child(name)?;
        Ok(())
    }

    fn rename(
        &self,
        src_name: &str,
        dst_dir: &axfs_ng_vfs::DirNode,
        dst_name: &str,
    ) -> axfs_ng_vfs::VfsResult<()> {
        let entry = self.remove_child(src_name)?;
        if let Ok(existing) = dst_dir.lookup(dst_name) {
            if existing.node_type() == axfs_ng_vfs::NodeType::Directory {
                return Err(ax_errno::AxError::IsADirectory);
            }
            dst_dir.unlink(dst_name, false)?;
        }
        dst_dir.link(dst_name, &entry)?;
        Ok(())
    }
}

fn more_metadata(inode: u64, node_type: axfs_ng_vfs::NodeType) -> axfs_ng_vfs::Metadata {
    axfs_ng_vfs::Metadata {
        device: 0,
        inode,
        nlink: 1,
        mode: axfs_ng_vfs::NodePermission::default(),
        node_type,
        uid: 0,
        gid: 0,
        size: 0,
        block_size: 4096,
        blocks: 0,
        rdev: axfs_ng_vfs::DeviceId::default(),
        atime: Duration::ZERO,
        mtime: Duration::ZERO,
        ctime: Duration::ZERO,
    }
}

fn new_more_root(inode: u64, child_dirs: &[&str], child_files: &[&str]) -> axfs_ng_vfs::DirEntry {
    let root = axfs_ng_vfs::DirEntry::new_dir(
        |weak| {
            axfs_ng_vfs::DirNode::new(Arc::new(MoreTestDir {
                inode,
                self_ref: weak,
                children: axfs_ng_vfs::Mutex::new(Vec::new()),
                next_inode: AtomicU64::new(inode * 10),
            }))
        },
        axfs_ng_vfs::Reference::root(),
    );
    let root_dir = root.as_dir().unwrap();
    for name in child_dirs {
        root_dir
            .create(
                name,
                axfs_ng_vfs::NodeType::Directory,
                axfs_ng_vfs::NodePermission::default(),
                0,
                0,
            )
            .unwrap();
    }
    for name in child_files {
        root_dir
            .create(
                name,
                axfs_ng_vfs::NodeType::RegularFile,
                axfs_ng_vfs::NodePermission::default(),
                0,
                0,
            )
            .unwrap();
    }
    root
}

fn new_more_fs(
    name: &'static str,
    readonly: bool,
    inode: u64,
    child_dirs: &[&str],
    child_files: &[&str],
) -> axfs_ng_vfs::Filesystem {
    axfs_ng_vfs::Filesystem::new(Arc::new(MoreTestFs {
        name,
        root: new_more_root(inode, child_dirs, child_files),
        readonly,
    }))
}

#[axtest::def_test]
fn axfs_ng_vfs_shared_mount_propagation_rules_hold() {
    use ax_errno::AxError;
    use axfs_ng_vfs::{Mountpoint, NodePermission, NodeType};

    let source_fs = new_more_fs("source-root", false, 100, &["target", "left"], &[]);
    let peer_fs = new_more_fs("peer-root", false, 200, &["target", "left"], &[]);
    let slave_fs = new_more_fs("slave-root", false, 300, &["target", "left"], &[]);
    let child_fs = new_more_fs("child", false, 400, &["inside"], &["leaf"]);

    let source_mount = Mountpoint::new_root(&source_fs);
    let peer_mount = Mountpoint::new_root(&peer_fs);
    let slave_mount = Mountpoint::new_root(&slave_fs);
    source_mount.set_shared();
    peer_mount.join_shared_group(&source_mount);
    slave_mount.join_shared_group(&source_mount);
    slave_mount.set_slave();

    ax_assert!(source_mount.is_shared());
    ax_assert!(peer_mount.is_shared());
    ax_assert!(slave_mount.is_slave());

    let source_root = source_mount.root_location();
    let peer_root = peer_mount.root_location();
    let slave_root = slave_mount.root_location();
    let source_target = source_root.lookup_no_follow("target").unwrap();
    let peer_target = peer_root.lookup_no_follow("target").unwrap();
    let slave_target = slave_root.lookup_no_follow("target").unwrap();
    let child_mount = source_target.mount(&child_fs).unwrap();

    ax_assert!(source_target.is_mountpoint());
    ax_assert!(peer_target.is_mountpoint());
    ax_assert!(slave_target.is_mountpoint());
    ax_assert_eq!(
        source_root
            .lookup_no_follow("target")
            .unwrap()
            .mountpoint()
            .device(),
        child_mount.device()
    );
    ax_assert_eq!(
        peer_root
            .lookup_no_follow("target")
            .unwrap()
            .mountpoint()
            .device(),
        child_mount.device()
    );
    ax_assert_eq!(
        slave_root
            .lookup_no_follow("target")
            .unwrap()
            .mountpoint()
            .device(),
        child_mount.device()
    );
    ax_assert_eq!(
        peer_root
            .lookup_no_follow("target")
            .unwrap()
            .absolute_path()
            .unwrap()
            .as_str(),
        "/target"
    );
    ax_assert!(
        peer_root
            .lookup_no_follow("target")
            .unwrap()
            .is_root_of_mount()
    );
    ax_assert!(
        slave_root
            .lookup_no_follow("target")
            .unwrap()
            .is_root_of_mount()
    );

    ax_assert!(matches!(
        source_target.create(
            "blocked",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        ),
        Err(AxError::ReadOnlyFilesystem) | Ok(_)
    ));
}

#[axtest::def_test]
fn axfs_ng_vfs_bind_mount_propagation_and_unbindable_rules_hold() {
    use ax_errno::AxError;
    use axfs_ng_vfs::Mountpoint;

    let source_fs = new_more_fs("bind-source", false, 500, &["sub", "skip"], &[]);
    let target_fs = new_more_fs("bind-target", false, 600, &["bind", "recursive"], &[]);
    let nested_fs = new_more_fs("nested", false, 700, &["deep"], &[]);
    let skipped_fs = new_more_fs("skipped", false, 800, &["hidden"], &[]);

    let source_mount = Mountpoint::new_root(&source_fs);
    let source_root = source_mount.root_location();
    let nested_mount = source_root
        .lookup_no_follow("sub")
        .unwrap()
        .mount(&nested_fs)
        .unwrap();
    let skipped_mount = source_root
        .lookup_no_follow("skip")
        .unwrap()
        .mount(&skipped_fs)
        .unwrap();
    skipped_mount.set_unbindable();
    ax_assert!(skipped_mount.is_unbindable());

    let target_mount = Mountpoint::new_root(&target_fs);
    let target_root = target_mount.root_location();
    let bind_mount = target_root
        .lookup_no_follow("recursive")
        .unwrap()
        .bind_mount(&source_root, true)
        .unwrap();
    ax_assert_eq!(bind_mount.device(), source_mount.device());
    ax_assert_eq!(bind_mount.children().len(), 1);
    let bound_root = target_root.lookup_no_follow("recursive").unwrap();
    ax_assert!(
        bound_root
            .lookup_no_follow("sub")
            .unwrap()
            .is_root_of_mount()
    );
    ax_assert!(
        !bound_root
            .lookup_no_follow("skip")
            .unwrap()
            .is_root_of_mount()
    );
    ax_assert_eq!(
        bound_root
            .lookup_no_follow("sub")
            .unwrap()
            .mountpoint()
            .device(),
        nested_mount.device()
    );

    source_mount.set_shared();
    let shared_target_fs = new_more_fs("shared-bind-target", false, 900, &["bind"], &[]);
    let shared_target_mount = Mountpoint::new_root(&shared_target_fs);
    let shared_bind = shared_target_mount
        .root_location()
        .lookup_no_follow("bind")
        .unwrap()
        .bind_mount(&source_root, false)
        .unwrap();
    ax_assert!(shared_bind.is_shared());

    shared_bind.set_slave();
    ax_assert!(shared_bind.is_slave());
    let slave_target_fs = new_more_fs("slave-bind-target", false, 1000, &["bind"], &[]);
    let slave_target_mount = Mountpoint::new_root(&slave_target_fs);
    let slave_bind = slave_target_mount
        .root_location()
        .lookup_no_follow("bind")
        .unwrap()
        .bind_mount(
            &shared_target_mount
                .root_location()
                .lookup_no_follow("bind")
                .unwrap(),
            false,
        )
        .unwrap();
    ax_assert!(slave_bind.is_slave());

    ax_assert!(matches!(
        target_root
            .lookup_no_follow("bind")
            .unwrap()
            .bind_mount(&skipped_mount.root_location(), false),
        Err(AxError::InvalidInput)
    ));
}

#[axtest::def_test]
fn axfs_ng_vfs_mount_move_detach_and_error_rules_hold() {
    use ax_errno::AxError;
    use axfs_ng_vfs::{Mountpoint, NodePermission, NodeType};

    let root_fs = new_more_fs(
        "move-root",
        false,
        1100,
        &["first", "second", "plain", "nested-target"],
        &["file-target"],
    );
    let child_fs = new_more_fs("move-child", false, 1200, &["inner"], &[]);
    let busy_fs = new_more_fs("busy-child", false, 1250, &[], &[]);
    let root_mount = Mountpoint::new_root(&root_fs);
    let root = root_mount.root_location();
    let child_mount = root
        .lookup_no_follow("first")
        .unwrap()
        .mount(&child_fs)
        .unwrap();
    let busy_target = root.lookup_no_follow("nested-target").unwrap();
    busy_target.mount(&busy_fs).unwrap();
    let child_root = root.lookup_no_follow("first").unwrap();

    ax_assert!(matches!(
        root.lookup_no_follow("plain")
            .unwrap()
            .move_mount(&root.lookup_no_follow("second").unwrap()),
        Err(AxError::InvalidInput)
    ));
    ax_assert!(matches!(
        child_root.move_mount(&busy_target),
        Err(AxError::ResourceBusy)
    ));
    ax_assert!(matches!(
        child_root.move_mount(&child_root.lookup_no_follow("inner").unwrap()),
        Err(AxError::FilesystemLoop)
    ));
    ax_assert!(matches!(
        child_root.move_mount(&root.lookup_no_follow("file-target").unwrap()),
        Err(AxError::NotADirectory)
    ));

    child_root
        .move_mount(&root.lookup_no_follow("second").unwrap())
        .unwrap();
    ax_assert!(!root.lookup_no_follow("first").unwrap().is_root_of_mount());
    let moved = root.lookup_no_follow("second").unwrap();
    ax_assert!(moved.is_root_of_mount());
    ax_assert_eq!(moved.mountpoint().device(), child_mount.device());
    ax_assert_eq!(moved.name(), "second");
    ax_assert_eq!(
        moved.parent().unwrap().absolute_path().unwrap().as_str(),
        "/"
    );
    moved.detach_mount().unwrap();
    ax_assert!(!root.lookup_no_follow("second").unwrap().is_root_of_mount());

    let readonly_fs = new_more_fs("readonly-root", true, 1300, &["mnt"], &["file"]);
    let readonly_mount = Mountpoint::new_root(&readonly_fs);
    let readonly_root = readonly_mount.root_location();
    ax_assert!(readonly_root.is_readonly());
    ax_assert!(matches!(
        readonly_root.update_metadata(axfs_ng_vfs::MetadataUpdate::default()),
        Err(AxError::ReadOnlyFilesystem)
    ));
    ax_assert!(matches!(
        readonly_root.create(
            "new",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
        ),
        Err(AxError::ReadOnlyFilesystem)
    ));
    ax_assert!(matches!(
        readonly_root.link("linked", &root.lookup_no_follow("plain").unwrap()),
        Err(AxError::ReadOnlyFilesystem)
    ));
    ax_assert!(matches!(
        readonly_root.rename("file", &readonly_root, "renamed"),
        Err(AxError::ReadOnlyFilesystem)
    ));
    ax_assert!(matches!(
        readonly_root.unlink("file", false),
        Err(AxError::ReadOnlyFilesystem)
    ));
}

#[axtest::def_test]
fn axfs_ng_vfs_location_link_rename_and_transient_rules_hold() {
    use ax_errno::AxError;
    use axfs_ng_vfs::{Mountpoint, NodePermission, NodeType, OpenOptions};

    let left_fs = new_more_fs(
        "left-root",
        false,
        1400,
        &["dir", "other", "mount-target"],
        &["file", "replace"],
    );
    let right_fs = new_more_fs("right-root", false, 1500, &["dir"], &["file"]);
    let mounted_fs = new_more_fs("mounted-root", false, 1600, &["inside"], &[]);
    let left_mount = Mountpoint::new_root(&left_fs);
    let right_mount = Mountpoint::new_root(&right_fs);
    let left_root = left_mount.root_location();
    let right_root = right_mount.root_location();

    let file = left_root.lookup_no_follow("file").unwrap();
    let linked = left_root.link("linked", &file).unwrap();
    ax_assert_eq!(linked.name(), "linked");
    ax_assert!(left_root.lookup_no_follow("linked").unwrap().is_file());
    ax_assert!(matches!(
        left_root.link("cross", &right_root.lookup_no_follow("file").unwrap()),
        Err(AxError::CrossesDevices)
    ));

    left_root.rename("linked", &left_root, "renamed").unwrap();
    ax_assert!(matches!(
        left_root.lookup_no_follow("linked"),
        Err(AxError::NotFound)
    ));
    ax_assert!(left_root.lookup_no_follow("renamed").unwrap().is_file());
    left_root.rename("renamed", &left_root, "replace").unwrap();
    ax_assert!(left_root.lookup_no_follow("replace").unwrap().is_file());
    ax_assert!(matches!(
        left_root.rename("dir", &left_root, "replace"),
        Err(AxError::IsADirectory) | Err(AxError::AlreadyExists)
    ));
    ax_assert!(matches!(
        left_root.rename("file", &right_root, "cross"),
        Err(AxError::CrossesDevices)
    ));

    let dir = left_root.lookup_no_follow("dir").unwrap();
    let subdir = dir
        .create(
            "child",
            NodeType::Directory,
            NodePermission::default(),
            0,
            0,
        )
        .unwrap();
    ax_assert!(matches!(
        left_root.rename("dir", &subdir, "loop"),
        Err(AxError::InvalidInput)
    ));

    let options = OpenOptions {
        create: false,
        node_type: NodeType::Directory,
        ..Default::default()
    };
    ax_assert!(left_root.open_file("dir", &options).unwrap().is_dir());
    let options = OpenOptions {
        create: true,
        node_type: NodeType::Symlink,
        ..Default::default()
    };
    ax_assert!(left_root.open_file("new-link", &options).unwrap().is_file());

    left_root
        .lookup_no_follow("mount-target")
        .unwrap()
        .mount(&mounted_fs)
        .unwrap();
    let mounted = left_root.lookup_no_follow("mount-target").unwrap();
    ax_assert!(mounted.is_root_of_mount());
    ax_assert_eq!(
        mounted.lookup_no_follow(".").unwrap().inode(),
        mounted.inode()
    );
    ax_assert_eq!(
        mounted
            .lookup_no_follow("..")
            .unwrap()
            .absolute_path()
            .unwrap()
            .as_str(),
        "/"
    );

    let readonly_fs = new_more_fs("transient-root", true, 1700, &["existing"], &["file"]);
    let readonly_mount = Mountpoint::new_root(&readonly_fs);
    let readonly_root = readonly_mount.root_location();
    let existing = readonly_root
        .create_transient_mount_dir("existing", NodePermission::default(), 1, 2)
        .unwrap();
    ax_assert!(existing.is_dir());
    let transient = readonly_root
        .create_transient_mount_dir("file", NodePermission::default(), 1, 2)
        .unwrap();
    ax_assert!(transient.is_dir());
    ax_assert_eq!(transient.name(), "file");
    ax_assert!(matches!(
        readonly_root.create_transient_mount_dir("bad/name", NodePermission::default(), 1, 2),
        Err(AxError::InvalidInput)
    ));
}
