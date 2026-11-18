use alloc::sync::Arc;

use axfs_ng_vfs::{
    Location, MutationCredentials, NodeFlags, NodePermission, NodeType, PathHandle, VfsError,
    VfsResult, path::Path,
};

use super::{
    WriteAccess,
    handle::{File, FileBackend},
};
#[cfg(test)]
use crate::fs_core::FsContext;
use crate::fs_core::FsView;

bitflags::bitflags! {
    /// Flags describing the access mode of an opened file.
    #[derive(Debug, Clone, Copy)]
    pub struct FileFlags: u8 {
        /// Read access.
        const READ = 1;
        /// Write access.
        const WRITE = 2;
        /// Execute access.
        const EXECUTE = 4;
        /// Append mode — writes always go to the end of the file.
        const APPEND = 8;
        /// Path-only handle, no actual I/O is permitted.
        const PATH = 16;
    }
}

/// Results returned by [`OpenOptions::open`].
pub enum OpenResult {
    /// The opened path is a regular file.
    File(File),
    /// The opened path is a directory.
    Dir(PathHandle),
}

impl OpenResult {
    /// Converts into a [`File`], returning an error if this is a directory.
    pub fn into_file(self) -> VfsResult<File> {
        match self {
            Self::File(file) => Ok(file),
            Self::Dir(dir) => {
                drop(dir);
                Err(VfsError::IsADirectory)
            }
        }
    }

    /// Converts into a [`Location`], returning an error if this is a file.
    #[cfg(feature = "vfs")]
    pub fn into_dir(self) -> VfsResult<PathHandle> {
        match self {
            Self::Dir(dir) => Ok(dir),
            Self::File(_) => Err(VfsError::NotADirectory),
        }
    }

    /// Keeps the selected path use regardless of the opened object type.
    #[cfg(feature = "vfs")]
    pub fn into_path(self) -> PathHandle {
        match self {
            Self::File(file) => file.path_handle().clone(),
            Self::Dir(dir) => dir,
        }
    }
}

/// Options and flags which can be used to configure how a file is opened.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    // generic
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
    directory: bool,
    no_follow: bool,
    direct: bool,
    user: Option<(u32, u32)>,
    path: bool,
    node_type: NodeType,
    // system-specific
    mode: u32,
}

impl OpenOptions {
    /// Creates a blank new set of options ready for configuration.
    pub fn new() -> Self {
        Self {
            // generic
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
            directory: false,
            no_follow: false,
            direct: false,
            user: None,
            path: false,
            node_type: NodeType::RegularFile,
            // system-specific
            mode: 0o666,
        }
    }

    /// Sets the option for read access.
    pub fn read(&mut self, read: bool) -> &mut Self {
        self.read = read;
        self
    }

    /// Sets the option for write access.
    pub fn write(&mut self, write: bool) -> &mut Self {
        self.write = write;
        self
    }

    /// Sets the option for the append mode.
    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    /// Sets the option for truncating a previous file.
    pub fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.truncate = truncate;
        self
    }

    /// Sets the option to create a new file, or open it if it already exists.
    pub fn create(&mut self, create: bool) -> &mut Self {
        self.create = create;
        self
    }

    /// Sets the option to create a new file, failing if it already exists.
    pub fn create_new(&mut self, create_new: bool) -> &mut Self {
        self.create_new = create_new;
        self
    }

    /// Sets the option to open directory instead.
    pub fn directory(&mut self, directory: bool) -> &mut Self {
        self.directory = directory;
        self
    }

    /// Sets the option to not follow symlinks.
    #[cfg(feature = "vfs")]
    pub fn no_follow(&mut self, no_follow: bool) -> &mut Self {
        self.no_follow = no_follow;
        self
    }

    /// Sets the option to open the file with direct I/O.\
    #[cfg(feature = "vfs")]
    pub fn direct(&mut self, direct: bool) -> &mut Self {
        self.direct = direct;
        self
    }

    /// Sets the user and group id to open the file with.
    #[cfg(feature = "vfs")]
    pub fn user(&mut self, uid: u32, gid: u32) -> &mut Self {
        self.user = Some((uid, gid));
        self
    }

    /// Sets the option for path only access.
    #[cfg(feature = "vfs")]
    pub fn path(&mut self, path: bool) -> &mut Self {
        self.path = path;
        self
    }

    /// Sets the node type for the file.
    ///
    /// This will only be used if the file is created.
    #[cfg(feature = "vfs")]
    pub fn node_type(&mut self, node_type: NodeType) -> &mut Self {
        self.node_type = node_type;
        self
    }

    /// Sets the mode bits that a new file will be created with.
    #[cfg(feature = "vfs")]
    pub fn mode(&mut self, mode: u32) -> &mut Self {
        self.mode = mode;
        self
    }

    fn _open(
        &self,
        loc: Location,
        credentials: &MutationCredentials<'_>,
        newly_created: bool,
    ) -> VfsResult<OpenResult> {
        let flags = self.to_flags()?;

        // O_CREAT on an existing directory → EISDIR (Linux behavior;
        // CREAT carries an implicit "create regular file" intent that
        // conflicts with an existing directory regardless of access mode).
        // Fixes bug-open-creat-on-existing-dir-no-eisdir.
        // O_PATH path bypasses this — it doesn't actually open / mutate.
        if self.create && loc.is_dir() && !self.path {
            return Err(VfsError::IsADirectory);
        }

        if loc.is_readonly()
            && (flags.intersects(FileFlags::WRITE | FileFlags::APPEND) || self.truncate)
        {
            return Err(VfsError::ReadOnlyFilesystem);
        }

        if self.directory {
            loc.check_is_dir()?;
        }

        if loc.node_type() == NodeType::Symlink && !self.path {
            return Err(VfsError::FilesystemLoop);
        }

        // ENXIO on opening a UNIX-domain-socket file. man 2 open §"ENXIO":
        // "The file is a UNIX domain socket." Two exclusions:
        //   (1) O_PATH bypass: socket file can still be O_PATH-opened to get a
        //       location handle.
        //   (2) Caller intends to create a socket (self.node_type == Socket,
        //       used by axnet UnixSocket::bind which mounts /dev/log etc.)
        //       — opening a freshly-created Socket via the create-then-open
        //       path is internal kernel use, not user open(2).
        // Fixes bug-open-unix-socket-no-enxio.
        if !self.path
            && self.node_type != NodeType::Socket
            && loc.metadata()?.node_type == NodeType::Socket
        {
            return Err(VfsError::NoSuchDeviceOrAddress);
        }

        Ok(if loc.is_dir() {
            if self.truncate {
                return Err(VfsError::IsADirectory);
            }
            if flags.contains(FileFlags::WRITE) {
                return Err(VfsError::IsADirectory);
            }
            if !newly_created {
                self.check_open_access(&loc, &flags, credentials)?;
            }
            OpenResult::Dir(PathHandle::acquire(loc)?)
        } else {
            if !newly_created {
                self.check_open_access(&loc, &flags, credentials)?;
            }
            // Acquire before truncation and retain the lease for writable open
            // descriptions. O_RDONLY|O_TRUNC needs only a temporary lease.
            let write_access = if !self.path
                && loc.node_type() == NodeType::RegularFile
                && (flags.contains(FileFlags::WRITE) || self.truncate)
            {
                Some(Arc::new(WriteAccess::acquire(loc.clone())?))
            } else {
                None
            };
            if !newly_created
                && loc.filesystem().stack_depth() != 0
                && let Some(write) = &write_access
            {
                let mut access = NodePermission::OTHER_WRITE;
                if flags.contains(FileFlags::READ) {
                    access.insert(NodePermission::OTHER_READ);
                }
                write.prepare_open(access, credentials)?;
            }
            // TODO(mivik): is this correct?
            let non_cacheable_type = matches!(
                loc.metadata()?.node_type,
                NodeType::CharacterDevice | NodeType::Fifo | NodeType::Socket
            );

            let direct = non_cacheable_type
                || self.path
                || self.direct
                || loc.flags().contains(NodeFlags::NON_CACHEABLE);
            // O_TRUNC changes regular file contents; opening a pipe or device
            // must not call its length mutation operation.
            let truncate = self.truncate && !self.path && loc.node_type() == NodeType::RegularFile;
            let mut backend = if loc.filesystem().stack_depth() != 0
                && let Some(write) = &write_access
            {
                FileBackend::new_layered(loc, write, self.direct)?
            } else if !direct || loc.flags().contains(NodeFlags::ALWAYS_CACHE) {
                FileBackend::new_cached(loc)?
            } else {
                FileBackend::new_direct(loc)?
            };
            if truncate {
                backend.set_len(0)?;
            }
            if !flags.contains(FileFlags::WRITE) {
                backend.finish_readonly_open();
            }
            let mut file = File::new(backend, flags);
            if flags.contains(FileFlags::WRITE) {
                file.write_access = write_access;
            }
            OpenResult::File(file)
        })
    }

    /// Opens an already resolved cache-only path with the captured DAC identity.
    ///
    /// Path-only descriptions retain the original path. Readonly directories
    /// and regular memory files use explicit cached attributes and existing
    /// inode data. Unsupported admission or missing capabilities returns
    /// `WouldBlock`; final symlinks and sockets retain their type errors.
    /// This never invokes ordinary open or creates a data cache.
    #[cfg(feature = "vfs")]
    pub fn open_cached_path(
        &self,
        path: PathHandle,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<OpenResult> {
        if !self.is_valid() {
            return Err(VfsError::InvalidInput);
        }
        let location = path.location();
        if self.directory && !location.is_dir() {
            return Err(VfsError::NotADirectory);
        }
        if self.path {
            return Ok(if location.is_dir() {
                OpenResult::Dir(path)
            } else {
                OpenResult::File(File::path_only(path))
            });
        }
        if !self.read
            || self.write
            || self.append
            || self.truncate
            || self.create
            || self.create_new
            || self.direct
        {
            return Err(VfsError::WouldBlock);
        }
        match location.node_type() {
            NodeType::Symlink => return Err(VfsError::FilesystemLoop),
            NodeType::Socket => return Err(VfsError::NoSuchDeviceOrAddress),
            NodeType::RegularFile | NodeType::Directory => {}
            _ => return Err(VfsError::WouldBlock),
        }
        credentials.check_permission(
            &location.entry().cached_metadata()?,
            NodePermission::OTHER_READ,
        )?;
        Ok(if location.is_dir() {
            OpenResult::Dir(path)
        } else {
            let cached = super::cache::CachedFile::try_existing_memory(path)?;
            OpenResult::File(File::from_cached_readonly(cached))
        })
    }

    /// Opens a file at the given [`Location`] using these options.
    #[cfg(feature = "vfs")]
    pub fn open_loc(&self, loc: Location) -> VfsResult<OpenResult> {
        self.open_loc_with_credentials(loc, &MutationCredentials::root())
    }

    /// Opens an already-resolved location using the supplied DAC credentials.
    #[cfg(feature = "vfs")]
    pub fn open_loc_with_credentials(
        &self,
        loc: Location,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<OpenResult> {
        if !self.is_valid() {
            return Err(VfsError::InvalidInput);
        }
        self._open(loc, credentials, false)
    }

    /// Opens a path using the identity and paths selected for this operation.
    pub fn open(&self, context: &FsView<'_>, path: impl AsRef<Path>) -> VfsResult<OpenResult> {
        let credentials = context.credentials();
        if !self.is_valid() {
            return Err(VfsError::InvalidInput);
        }
        let flags = self.to_flags()?;
        let mut access = NodePermission::empty();
        if !self.path {
            if flags.contains(FileFlags::READ) {
                access.insert(NodePermission::OTHER_READ);
            }
            if flags.contains(FileFlags::WRITE) || self.truncate {
                access.insert(NodePermission::OTHER_WRITE);
            }
        }
        let intent = crate::fs_core::LookupOpenIntent {
            node: axfs_ng_vfs::OpenOptions {
                create: self.create,
                create_new: self.create_new,
                node_type: self.node_type,
                permission: NodePermission::from_bits_truncate(self.mode as _),
                user: self.user,
                access,
                observation: None,
            },
            follow: !self.no_follow,
            path_only: self.path,
            directory: self.directory,
        };
        let resolved = crate::fs_core::LookupSession::new(context, path.as_ref().as_str())?
            .parent()?
            .finish_open(intent)?;
        // Keep the accepted mount use until the opened description has acquired
        // its own path and writer rights, including on every failure path.
        self._open(
            resolved.path.location().clone(),
            credentials,
            resolved.created,
        )
    }

    fn check_open_access(
        &self,
        location: &Location,
        flags: &FileFlags,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        // O_PATH obtains a handle without opening the inode for I/O. Path
        // traversal has already checked search permission separately.
        if self.path {
            return Ok(());
        }

        let mut required = NodePermission::empty();
        if flags.contains(FileFlags::READ) {
            required.insert(NodePermission::OTHER_READ);
        }
        if flags.contains(FileFlags::WRITE) || self.truncate {
            required.insert(NodePermission::OTHER_WRITE);
        }
        if !required.is_empty() {
            FsView::check_permission(location, credentials, required)?;
        }
        Ok(())
    }

    pub(crate) fn to_flags(&self) -> VfsResult<FileFlags> {
        // Linux semantic: O_APPEND only adds APPEND bit; it does NOT promote
        // read-only fd to read/write. (Previous code merged (true,_,true) →
        // READ|WRITE|APPEND which silently upgraded RDONLY|APPEND to RW —
        // see bug-open-rdonly-append-promotes-rw.)
        Ok(match (self.read, self.write, self.append) {
            (true, false, false) => FileFlags::READ,
            (false, true, false) => FileFlags::WRITE,
            (true, true, false) => FileFlags::READ | FileFlags::WRITE,
            (true, false, true) => FileFlags::READ | FileFlags::APPEND,
            (false, true, true) => FileFlags::WRITE | FileFlags::APPEND,
            (true, true, true) => FileFlags::READ | FileFlags::WRITE | FileFlags::APPEND,
            (false, false, true) => FileFlags::APPEND, // RDONLY-equivalent + APPEND: pure status
            (false, false, false) => return Err(VfsError::InvalidInput),
        } | if self.path {
            FileFlags::PATH
        } else {
            FileFlags::empty()
        })
    }

    pub(crate) fn is_valid(&self) -> bool {
        if !self.read && !self.write && !self.append {
            return false;
        }
        // Linux multi-fs: RDONLY|TRUNC truncates the file (POSIX VERSIONS
        // says effect is "unspecified", but most fs do truncate). Don't
        // reject. Fixes bug-open-rdonly-trunc-einval.
        // RDWR|APPEND|TRUNC is also explicitly allowed by Linux; the prior
        // restriction "(_,true) && truncate && !create_new → false" was too
        // strict. Fixes bug-open-append-trunc-einval.
        true
    }
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::{any::Any, time::Duration};

    use axfs_ng_vfs::{
        DeviceId, DirEntry, FileNode, FileNodeOps, Filesystem, FilesystemOps, Metadata,
        MetadataUpdate, Mountpoint, NodeOps, NodePermission, NodeType, Reference, StatFs, VfsError,
    };
    use axpoll::{IoEvents, Pollable};

    use super::*;

    #[test]
    fn opened_paths_keep_mount_uses_through_dup_close_and_lazy_detach() {
        use alloc::{boxed::Box, rc::Rc};

        use axfs_ng_vfs::UnmountKind;

        for kind in [
            "direct",
            "buffered",
            "path",
            "directory",
            "dirfd",
            "iterator",
        ] {
            let tree = Mountpoint::new_root(&crate::MemoryFs::new());
            let view = FsContext::from_tree(tree.clone(), tree.root_location()).unwrap();
            let view_caller = axfs_ng_vfs::NativeLinkCaller::root();
            let view = crate::FsView::native(view, &view_caller);
            let point = view
                .create_dir("/mount", NodePermission::default(), 0, 0)
                .unwrap();
            let mounted = point.location().mount(&crate::MemoryFs::new()).unwrap();
            let root = mounted.root_location();
            let location = root
                .create(
                    "file",
                    NodeType::RegularFile,
                    NodePermission::default(),
                    0,
                    0,
                    &MutationCredentials::root(),
                )
                .unwrap();
            let open = || -> Box<dyn Any> {
                match kind {
                    "direct" => Box::new(File::new(
                        FileBackend::new_direct(location.clone()).unwrap(),
                        FileFlags::READ,
                    )),
                    "buffered" | "path" => {
                        let mut options = OpenOptions::new();
                        options.read(true);
                        options.path = kind == "path";
                        Box::new(
                            options
                                ._open(location.clone(), &MutationCredentials::root(), false)
                                .unwrap()
                                .into_file()
                                .unwrap(),
                        )
                    }
                    "directory" => Box::new(
                        crate::fops::Directory::from_path(
                            PathHandle::acquire(root.clone()).unwrap(),
                        )
                        .unwrap(),
                    ),
                    "dirfd" => Box::new(
                        crate::fops::Directory::from_path(
                            PathHandle::acquire(root.clone()).unwrap(),
                        )
                        .unwrap()
                        .dir_handle(),
                    ),
                    "iterator" => Box::new(view.read_dir("/mount").unwrap()),
                    _ => unreachable!(),
                }
            };
            let held = Rc::new(open());
            assert!(
                matches!(
                    mounted.plan_unmount(UnmountKind::Normal),
                    Err(VfsError::ResourceBusy)
                ),
                "{kind}: an open path must pin its exact mount"
            );
            let duplicate = held.clone();
            drop(held);
            assert!(
                matches!(
                    mounted.plan_unmount(UnmountKind::Normal),
                    Err(VfsError::ResourceBusy)
                ),
                "{kind}: duplicate ownership keeps the use"
            );
            drop(duplicate);
            let admitted = mounted.plan_unmount(UnmountKind::Normal).unwrap();
            let mut held = open();
            assert_eq!(
                root.commit_unmount(admitted),
                Err(VfsError::ResourceBusy),
                "{kind}: commit rechecks a newly opened path"
            );
            root.detach_mount().unwrap();
            if let Some(file) = held.downcast_ref::<File>() {
                assert_eq!(
                    file.location().metadata().unwrap().node_type,
                    NodeType::RegularFile
                );
            } else if let Some(directory) = held.downcast_ref::<crate::fops::Directory>() {
                assert_eq!(directory.get_attr().unwrap().node_type, NodeType::Directory);
            } else if let Some(directory) = held.downcast_ref::<crate::fops::DirHandle>() {
                assert_eq!(
                    directory
                        .path_handle()
                        .location()
                        .metadata()
                        .unwrap()
                        .node_type,
                    NodeType::Directory
                );
            } else if let Some(iterator) = held.downcast_mut::<crate::fs_core::ReadDir>() {
                assert!(iterator.any(|entry| entry.unwrap().name == "file"));
            } else {
                unreachable!("known open path kind");
            }
        }
    }

    struct TestFilesystem {
        this: alloc::sync::Weak<Self>,
        depth: u32,
        reads: alloc::vec::Vec<Location>,
        writes: alloc::vec::Vec<Location>,
    }

    impl TestFilesystem {
        const fn native() -> Self {
            Self {
                this: alloc::sync::Weak::new(),
                depth: 0,
                reads: alloc::vec::Vec::new(),
                writes: alloc::vec::Vec::new(),
            }
        }
    }

    struct TestFile {
        metadata: Metadata,
        filesystem: Option<Arc<TestFilesystem>>,
    }

    static TEST_FILESYSTEM: TestFilesystem = TestFilesystem::native();

    impl TestFile {
        fn backing_file(&self) -> VfsResult<Location> {
            let backing = self
                .filesystem
                .as_ref()
                .and_then(|filesystem| filesystem.writes.first())
                .ok_or(VfsError::BadState)?;
            if backing.is_dir() {
                backing.lookup_no_follow("writer")
            } else {
                Ok(backing.clone())
            }
        }
    }

    struct TestOpenAttempt<'a>(&'a TestFile);

    impl axfs_ng_vfs::LockedFileOpenAttempt for TestOpenAttempt<'_> {
        fn observe(&mut self) -> VfsResult<Metadata> {
            self.0.metadata()
        }

        fn commit(
            &mut self,
            open: axfs_ng_vfs::AuthorizedFileOpen<'_>,
        ) -> axfs_ng_vfs::NamespaceOutcome<Metadata> {
            let result = (|| {
                let backing = self.0.backing_file()?;
                let selected = open.forward_to(&backing, &MutationCredentials::root())?;
                selected.result()?;
                Ok(match selected.state() {
                    axfs_ng_vfs::NamespaceState::Retry => axfs_ng_vfs::NamespaceOutcome::retry(),
                    axfs_ng_vfs::NamespaceState::Unchanged => {
                        axfs_ng_vfs::NamespaceOutcome::unchanged_observed(
                            self.0.metadata()?,
                            Ok(()),
                        )
                    }
                    _ => axfs_ng_vfs::NamespaceOutcome::uncertain(None, Err(VfsError::BadState)),
                })
            })();
            result.unwrap_or_else(|error| axfs_ng_vfs::NamespaceOutcome::unchanged(Err(error)))
        }
    }

    impl FilesystemOps for TestFilesystem {
        fn name(&self) -> &str {
            "open-permission-test"
        }

        fn root_dir(&self) -> DirEntry {
            test_entry_on(
                metadata(0, 0, 0, NodePermission::default()),
                self.this.upgrade(),
            )
        }

        fn stack_depth(&self) -> u32 {
            self.depth
        }

        fn read_backing_roots(&self) -> &[Location] {
            &self.reads
        }

        fn write_backing_roots(&self) -> &[Location] {
            &self.writes
        }

        fn stat(&self) -> VfsResult<StatFs> {
            Err(VfsError::InvalidInput)
        }
    }

    impl NodeOps for TestFile {
        fn inode(&self) -> u64 {
            self.metadata.inode
        }

        fn metadata(&self) -> VfsResult<Metadata> {
            Ok(self.metadata.clone())
        }

        fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
            Ok(())
        }

        fn filesystem(&self) -> &dyn FilesystemOps {
            self.filesystem.as_deref().unwrap_or(&TEST_FILESYSTEM)
        }

        fn sync(&self, _data_only: bool) -> VfsResult<()> {
            Ok(())
        }

        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    impl Pollable for TestFile {
        fn poll(&self) -> IoEvents {
            IoEvents::IN | IoEvents::OUT
        }

        unsafe fn register_shared(
            &self,
            _sink: &mut dyn axpoll::SharedRegistrationSink,
            _events: IoEvents,
        ) {
        }
    }

    impl FileNodeOps for TestFile {
        fn prepare_write_open(
            &self,
            request: &axfs_ng_vfs::FileOpenRequest<'_>,
        ) -> axfs_ng_vfs::NamespaceCompletion<Metadata> {
            axfs_ng_vfs::NamespaceCompletion::new(request.execute(&mut TestOpenAttempt(self)), None)
        }

        fn resolve_file_io(
            &self,
            request: axfs_ng_vfs::FileIoRequest<'_>,
        ) -> VfsResult<axfs_ng_vfs::FileIoTarget> {
            if self.filesystem().stack_depth() == 0 {
                request.native(self)
            } else {
                request.forward_to(self.backing_file()?, &MutationCredentials::root())
            }
        }

        fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
            Ok(0)
        }

        fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
            Ok(0)
        }

        fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
            Ok((0, self.metadata.size))
        }

        fn set_len(&self, _len: u64) -> VfsResult<()> {
            Ok(())
        }
    }

    fn metadata(uid: u32, gid: u32, inode: u64, mode: NodePermission) -> Metadata {
        Metadata {
            device: 0,
            inode,
            nlink: 1,
            mode,
            node_type: NodeType::RegularFile,
            uid,
            gid,
            size: 0,
            block_size: 4096,
            blocks: 0,
            rdev: DeviceId::default(),
            atime: Duration::ZERO,
            mtime: Duration::ZERO,
            ctime: Duration::ZERO,
        }
    }

    fn test_entry(metadata: Metadata) -> DirEntry {
        test_entry_on(metadata, None)
    }

    fn test_entry_on(metadata: Metadata, filesystem: Option<Arc<TestFilesystem>>) -> DirEntry {
        DirEntry::new_file(
            FileNode::new(Arc::new(TestFile {
                metadata,
                filesystem,
            })),
            NodeType::RegularFile,
            Reference::root(),
        )
    }

    fn test_location(metadata: Metadata) -> Location {
        let filesystem = Filesystem::new(Arc::new(TestFilesystem::native()));
        Location::new(Mountpoint::new_root(&filesystem), test_entry(metadata))
    }

    fn credentials(fsuid: u32, fsgid: u32, supplementary_gids: &[u32]) -> MutationCredentials<'_> {
        MutationCredentials {
            cap_mknod: false,
            fsuid,
            fsgid,
            supplementary_gids,
            cap_dac_override: false,
            cap_dac_read_search: false,
            cap_fowner: false,
            cap_chown: false,
            cap_fsetid: false,
        }
    }

    #[cfg(feature = "vfs")]
    #[test]
    fn cached_readonly_requires_existing_contents_and_current_dac() {
        let filesystem = crate::MemoryFs::new();
        let root = axfs_ng_vfs::Mountpoint::new_root(&filesystem).root_location();
        let location = root
            .create(
                "private",
                NodeType::RegularFile,
                NodePermission::from_bits_truncate(0o600),
                1000,
                1000,
                &MutationCredentials {
                    fsuid: 1000,
                    fsgid: 1000,
                    ..MutationCredentials::root()
                },
            )
            .unwrap();
        let path = PathHandle::acquire(location.clone()).unwrap();
        let owner = credentials(1000, 1000, &[]);
        let mut options = OpenOptions::new();
        options.read(true);
        assert_eq!(
            options.open_cached_path(path.clone(), &owner).map(|_| ()),
            Err(VfsError::WouldBlock)
        );

        let ordinary = options
            .open_loc_with_credentials(location.clone(), &owner)
            .and_then(OpenResult::into_file)
            .unwrap();
        let cached = options
            .open_cached_path(path.clone(), &owner)
            .and_then(OpenResult::into_file)
            .unwrap();
        drop(ordinary);
        location
            .change_metadata(
                axfs_ng_vfs::MetadataChange::Chmod(NodePermission::empty()),
                &MutationCredentials::root(),
            )
            .unwrap();
        assert!(matches!(
            options.open_cached_path(path, &owner),
            Err(VfsError::PermissionDenied)
        ));
        let mut bytes = [0];
        assert_eq!(cached.read_at(&mut bytes[..], 0), Ok(0));
    }

    #[test]
    fn final_access_uses_owner_group_and_other_mode_bits() {
        let location = test_location(metadata(
            1000,
            2000,
            1,
            NodePermission::from_bits_truncate(0o642),
        ));

        assert!(
            OpenOptions::new()
                .write(true)
                .check_open_access(&location, &FileFlags::WRITE, &credentials(1000, 3000, &[]))
                .is_ok()
        );
        assert!(
            OpenOptions::new()
                .read(true)
                .check_open_access(
                    &location,
                    &FileFlags::READ,
                    &credentials(3000, 3001, &[2000])
                )
                .is_ok()
        );
        assert_eq!(
            OpenOptions::new().write(true).check_open_access(
                &location,
                &FileFlags::WRITE,
                &credentials(3000, 3001, &[2000])
            ),
            Err(VfsError::PermissionDenied)
        );
        assert!(
            OpenOptions::new()
                .write(true)
                .check_open_access(&location, &FileFlags::WRITE, &credentials(3000, 3001, &[]))
                .is_ok()
        );
        assert_eq!(
            OpenOptions::new().read(true).check_open_access(
                &location,
                &FileFlags::READ,
                &credentials(3000, 3001, &[])
            ),
            Err(VfsError::PermissionDenied)
        );
    }

    #[test]
    fn truncate_requires_write_and_path_skips_final_access() {
        let location = test_location(metadata(0, 0, 1, NodePermission::from_bits_truncate(0o400)));
        let mut truncate = OpenOptions::new();
        truncate.read(true).truncate(true);
        assert_eq!(
            truncate.check_open_access(&location, &FileFlags::READ, &credentials(1000, 1000, &[]),),
            Err(VfsError::PermissionDenied)
        );

        let mut path = OpenOptions::new();
        path.read(true).write(true).path = true;
        assert!(
            path.check_open_access(
                &location,
                &(FileFlags::READ | FileFlags::WRITE),
                &credentials(1000, 1000, &[]),
            )
            .is_ok()
        );
    }

    #[test]
    fn newly_created_inode_skips_final_access_check() {
        let location = test_location(metadata(0, 0, 1, NodePermission::from_bits_truncate(0)));
        let credentials = credentials(1000, 1000, &[]);
        let mut options = OpenOptions::new();
        options.read(true).direct = true;

        assert!(options._open(location.clone(), &credentials, true).is_ok());
        assert!(matches!(
            options._open(location, &credentials, false),
            Err(VfsError::PermissionDenied)
        ));
    }

    #[cfg(feature = "host-test")]
    fn check_backing_write_rights() {
        use alloc::{vec, vec::Vec};

        // Only the dependency declarations are a fixture. Every mount lease,
        // open description, duplicate and remount uses the real implementation.
        fn layer(depth: u32, reads: Vec<Location>, writes: Vec<Location>) -> Location {
            let filesystem = Filesystem::new(Arc::new_cyclic(|this| TestFilesystem {
                this: this.clone(),
                depth,
                reads,
                writes,
            }));
            Mountpoint::new_root(&filesystem).root_location()
        }

        let lower_fs = crate::MemoryFs::new();
        let lower = Mountpoint::new_root(&lower_fs).root_location();
        lower.mountpoint().set_readonly(true).unwrap();
        let upper_fs = crate::MemoryFs::new();
        let upper = Mountpoint::new_root(&upper_fs).root_location();
        upper
            .create(
                "writer",
                NodeType::RegularFile,
                NodePermission::from_bits_truncate(0o600),
                0,
                0,
                &MutationCredentials::root(),
            )
            .unwrap();
        let middle = layer(1, vec![lower.clone()], vec![upper.clone()]);
        let visible = layer(2, Vec::new(), vec![middle.clone()]);
        let credentials = MutationCredentials::root();
        let mut writable = OpenOptions::new();
        writable.read(true).write(true).direct = true;
        let description = Arc::new(
            writable
                ._open(visible.clone(), &credentials, false)
                .and_then(OpenResult::into_file)
                .unwrap(),
        );
        let duplicate = description.clone();
        let mapping = description.write_access().unwrap().clone();
        // The logical node owns no bytes. Ordinary I/O must use the selected
        // MemoryFs cache and retain that accepted endpoint after DAC changes.
        assert_eq!(description.write_at(b"original".as_slice(), 0), Ok(8));
        let native = upper.lookup_no_follow("writer").unwrap();
        native
            .change_metadata(
                axfs_ng_vfs::MetadataChange::Chmod(NodePermission::empty()),
                &credentials,
            )
            .unwrap();
        assert_eq!(description.write_at(b"updated".as_slice(), 0), Ok(7));
        let mut observed = [0; 8];
        assert_eq!(description.read_at(observed.as_mut_slice(), 0), Ok(8));
        assert_eq!(&observed, b"updatedl");
        let reject_readonly = || {
            assert!(
                matches!(
                    crate::file::ExecutableFile::acquire(native.clone()),
                    Err(VfsError::TextFileBusy)
                ),
                "backing execution stays excluded through the last mapping lease"
            );
            for location in [&visible, &middle, &upper] {
                assert_eq!(
                    location.mountpoint().set_readonly(true),
                    Err(VfsError::ResourceBusy),
                    "writable descriptions and mappings must retain every write dependency"
                );
            }
            assert_eq!(
                upper.mountpoint().set_filesystem_readonly(true),
                Err(VfsError::ResourceBusy)
            );
        };
        reject_readonly();
        drop(description);
        reject_readonly();
        drop(duplicate);
        reject_readonly();
        drop(mapping);
        let executable = crate::file::ExecutableFile::acquire(native.clone()).unwrap();
        assert!(matches!(
            writable._open(visible.clone(), &credentials, false),
            Err(VfsError::TextFileBusy)
        ));
        drop(executable);
        let mut truncate = OpenOptions::new();
        truncate.read(true).truncate(true).direct = true;
        let reader = truncate
            ._open(visible.clone(), &credentials, false)
            .and_then(OpenResult::into_file)
            .unwrap();
        assert!(reader.write_access().is_none());
        let executable = crate::file::ExecutableFile::acquire(native).unwrap();
        drop(executable);
        drop(reader);

        for location in [&visible, &middle, &upper] {
            location.mountpoint().set_readonly(true).unwrap();
        }
        // Refusing a later backing must release the earlier visible/middle
        // leases too. The read-only lower remains a read dependency throughout.
        visible.mountpoint().set_readonly(false).unwrap();
        middle.mountpoint().set_readonly(false).unwrap();
        assert!(matches!(
            writable._open(visible.clone(), &credentials, false),
            Err(VfsError::ReadOnlyFilesystem)
        ));
        visible.mountpoint().set_readonly(true).unwrap();
        middle.mountpoint().set_readonly(true).unwrap();
        assert!(lower.is_readonly());
    }

    #[cfg(feature = "host-test")]
    #[test]
    fn readonly_reconfiguration_preserves_writers_until_description_and_mapping_release() {
        crate::os::memory::test_support::with_test_page_provider(true, |_| {
            check_readonly_reconfiguration();
        });
    }

    #[cfg(feature = "host-test")]
    fn check_readonly_reconfiguration() {
        for shared in [false, true] {
            let filesystem = crate::MemoryFs::new();
            let mount = Mountpoint::new_root(&filesystem);
            let location = mount
                .root_location()
                .create(
                    "writer",
                    NodeType::RegularFile,
                    NodePermission::from_bits_truncate(0o600),
                    1000,
                    1000,
                    &axfs_ng_vfs::MutationCredentials {
                        fsuid: 1000,
                        fsgid: 1000,
                        ..axfs_ng_vfs::MutationCredentials::root()
                    },
                )
                .unwrap();
            let target = if shared {
                mount.clone_tree()
            } else {
                mount.clone()
            };
            let credentials = credentials(1000, 1000, &[]);
            let mut writable = OpenOptions::new();
            writable.write(true).direct = true;
            let description = Arc::new(
                writable
                    ._open(location.clone(), &credentials, false)
                    .and_then(OpenResult::into_file)
                    .unwrap(),
            );
            let duplicate = description.clone();
            let mapping = description.write_access().unwrap().clone();
            let reopened = writable
                ._open(location.clone(), &credentials, false)
                .and_then(OpenResult::into_file)
                .unwrap();
            // A bind-local change does not drain writers on another alias,
            // and those writers cannot lend authority to the read-only alias.
            let alias = mount.clone_tree();
            alias.set_readonly(true).unwrap();
            let alias_location = Location::new(alias.clone(), location.entry().clone());
            assert!(matches!(
                alias_location.acquire_write(),
                Err(VfsError::ReadOnlyFilesystem)
            ));
            {
                let right = location.acquire_write().unwrap();
                right.check_location(&location).unwrap();
                assert_eq!(
                    right.check_location(&alias_location),
                    Err(VfsError::CrossesDevices)
                );
            }
            // Keeping or relaxing write authority must not reject live writers.
            // Collect every result before asserting so one refusal cannot hide
            // the other entry points in the same regression run.
            let writable_reconfigurations = (
                mount.set_readonly(false),
                filesystem.set_readonly(false),
                mount.set_filesystem_readonly(false),
                alias.set_filesystem_readonly(false),
            );
            assert_eq!(
                writable_reconfigurations,
                (Ok(()), Ok(()), Ok(()), Ok(())),
                "local RW, shared RW, ordinary RW, ordinary RW clearing alias-local RO"
            );
            assert!(!mount.is_readonly());
            assert!(!filesystem.is_readonly());
            assert!(!alias.is_readonly());
            assert!(!alias.is_filesystem_readonly());
            let reject_readonly = || {
                if shared {
                    assert_eq!(
                        target.set_filesystem_readonly(true),
                        Err(VfsError::ResourceBusy)
                    );
                    assert!(!target.is_filesystem_readonly());
                    assert!(!target.is_readonly());
                } else {
                    assert_eq!(target.set_readonly(true), Err(VfsError::ResourceBusy));
                    assert!(!target.is_readonly());
                }
            };
            reject_readonly();
            drop(description);
            reject_readonly();
            drop(duplicate);
            drop(reopened);
            reject_readonly();
            drop(mapping);

            let mut read_only = OpenOptions::new();
            read_only.read(true).direct = true;
            let reader = read_only
                ._open(location.clone(), &credentials, false)
                .and_then(OpenResult::into_file)
                .unwrap();
            let mut path = read_only.clone();
            path.path = true;
            let path_file = path
                ._open(location.clone(), &credentials, false)
                .and_then(OpenResult::into_file)
                .unwrap();
            if shared {
                target.set_filesystem_readonly(true).unwrap();
                assert!(target.is_filesystem_readonly());
                assert!(target.is_readonly());
            } else {
                target.set_readonly(true).unwrap();
                assert!(target.is_readonly());
            }
            assert!(matches!(
                writable._open(location, &credentials, false),
                Err(VfsError::ReadOnlyFilesystem)
            ));
            drop(reader);
            drop(path_file);
        }

        check_backing_write_rights();

        let filesystem = crate::MemoryFs::new();
        let mount = Mountpoint::new_root(&filesystem);
        let root = mount.root_location();
        root.update_metadata(MetadataUpdate {
            mode: Some(NodePermission::from_bits_truncate(0o777)),
            ..Default::default()
        })
        .unwrap();
        let existing = root
            .create(
                "existing",
                NodeType::RegularFile,
                NodePermission::from_bits_truncate(0o600),
                1000,
                1000,
                &axfs_ng_vfs::MutationCredentials {
                    fsuid: 1000,
                    fsgid: 1000,
                    ..axfs_ng_vfs::MutationCredentials::root()
                },
            )
            .unwrap();
        let credentials = credentials(1000, 1000, &[]);
        let context = FsContext::from_tree(root.mountpoint().clone(), root.clone()).unwrap();
        let context_paths = context.clone();
        let context_caller = axfs_ng_vfs::NativeLinkCaller::new(credentials);
        let context = crate::FsView::native(context, &context_caller);
        let mut writable = OpenOptions::new();
        writable.read(true).write(true).direct = true;
        let writer = writable
            .open(&context, "existing")
            .and_then(OpenResult::into_file)
            .unwrap();
        assert_eq!(writer.write_at(b"retained".as_slice(), 0), Ok(8));
        drop(writer);
        let identity = existing.entry().key();
        root.change_metadata(
            axfs_ng_vfs::MetadataChange::Chmod(NodePermission::from_bits_truncate(0o555)),
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();
        mount.set_readonly(true).unwrap();

        let mut read_only = OpenOptions::new();
        read_only.read(true).direct = true;
        let mut path = read_only.clone();
        path.path = true;
        let mut create = read_only.clone();
        create.create(true);
        let mut exclusive = create.clone();
        exclusive.create_new(true);
        let ordinary = read_only
            .open(&context, "existing")
            .and_then(OpenResult::into_file);
        let path_only = path
            .open(&context, "existing")
            .and_then(OpenResult::into_file);
        let opened = create
            .open(&context, "existing")
            .and_then(OpenResult::into_file);
        let excluded = exclusive
            .open(&context, "existing")
            .and_then(OpenResult::into_file);
        let missing = create
            .open(&context, "missing")
            .and_then(OpenResult::into_file);
        // OpenResult hides the created marker. Exercise its real existing
        // status-bearing entry as well, without bypassing the creation request.
        let status = root.open_file_with_status(
            "existing",
            &axfs_ng_vfs::OpenOptions {
                access: axfs_ng_vfs::NodePermission::empty(),
                create: true,
                create_new: false,
                node_type: NodeType::RegularFile,
                permission: NodePermission::from_bits_truncate(0o600),
                user: Some((1000, 1000)),
                observation: None,
            },
            &axfs_ng_vfs::MutationCredentials::root(),
        );
        let missing_absent = matches!(root.lookup_no_follow("missing"), Err(VfsError::NotFound));
        assert_eq!(
            (
                ordinary.as_ref().map(|_| ()).map_err(|error| *error),
                path_only.as_ref().map(|_| ()).map_err(|error| *error),
                opened.as_ref().map(|_| ()).map_err(|error| *error),
                excluded.as_ref().map(|_| ()).map_err(|error| *error),
                missing.as_ref().map(|_| ()).map_err(|error| *error),
                status
                    .as_ref()
                    .map(|(_, created)| *created)
                    .map_err(|error| *error),
            ),
            (
                Ok(()),
                Ok(()),
                Ok(()),
                Err(VfsError::AlreadyExists),
                Err(VfsError::ReadOnlyFilesystem),
                Ok(false),
            ),
            "ordinary, O_PATH, existing O_CREAT, existing O_EXCL, missing O_CREAT, created status"
        );
        assert!(
            missing_absent,
            "a read-only creation failure must not publish a name"
        );
        assert!(mount.is_readonly());
        for reader in [ordinary.unwrap(), opened.unwrap()] {
            assert!(reader.location().entry().ptr_eq(existing.entry()));
            assert_eq!(reader.location().entry().key(), identity);
            assert!(reader.write_access().is_none());
            let mut bytes = [0; 8];
            assert_eq!(reader.read_at(&mut bytes[..], 0), Ok(8));
            assert_eq!(&bytes, b"retained");
        }
        let (location, created) = status.unwrap();
        assert!(!created);
        assert!(location.entry().ptr_eq(existing.entry()));
        assert_eq!(existing.metadata().unwrap().nlink, 1);

        // Searchable but non-writable parents do not block existing selections.
        // The current object's DAC still applies despite the saved write denial.
        let stranger = MutationCredentials {
            fsuid: 2000,
            fsgid: 2000,
            ..credentials
        };
        let stranger_caller = axfs_ng_vfs::NativeLinkCaller::new(stranger);
        let stranger_context = crate::FsView::native(context_paths, &stranger_caller);
        assert!(matches!(
            create.open(&stranger_context, "existing"),
            Err(VfsError::PermissionDenied)
        ));
        let path_created = root
            .open_file_with_status(
                "existing",
                &axfs_ng_vfs::OpenOptions {
                    create: true,
                    ..Default::default()
                },
                &axfs_ng_vfs::MutationCredentials {
                    fsuid: 2000,
                    fsgid: 2000,
                    cap_dac_override: false,
                    cap_dac_read_search: false,
                    ..axfs_ng_vfs::MutationCredentials::root()
                },
            )
            .unwrap();
        assert!(!path_created.1);
        assert!(path_created.0.entry().ptr_eq(existing.entry()));
        assert!(matches!(
            exclusive.open(&context, "existing"),
            Err(VfsError::AlreadyExists)
        ));
        mount.set_readonly(false).unwrap();
        assert!(matches!(
            create.open(&context, "denied"),
            Err(VfsError::PermissionDenied)
        ));
        assert!(matches!(
            root.lookup_no_follow("denied"),
            Err(VfsError::NotFound)
        ));
        root.change_metadata(
            axfs_ng_vfs::MetadataChange::Chmod(NodePermission::from_bits_truncate(0o444)),
            &axfs_ng_vfs::MutationCredentials::root(),
        )
        .unwrap();
        mount.set_readonly(true).unwrap();
        assert!(matches!(
            create.open(&context, "existing"),
            Err(VfsError::PermissionDenied)
        ));
        assert_eq!(existing.entry().key(), identity);
        assert_eq!(existing.metadata().unwrap().nlink, 1);
    }
}
