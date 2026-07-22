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
