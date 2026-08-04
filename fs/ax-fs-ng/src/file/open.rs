use axfs_ng_vfs::{Location, NodeFlags, NodePermission, NodeType, VfsError, VfsResult, path::Path};

use super::handle::{File, FileBackend};
use crate::fs_core::FsContext;

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
    Dir(Location),
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
    pub fn into_dir(self) -> VfsResult<Location> {
        match self {
            Self::Dir(dir) => Ok(dir),
            Self::File(_) => Err(VfsError::NotADirectory),
        }
    }

    /// Extracts the underlying [`Location`] regardless of variant.
    #[cfg(feature = "vfs")]
    pub fn into_location(self) -> Location {
        match self {
            Self::File(file) => file.location().clone(),
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
    #[cfg(feature = "vfs")]
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

    fn _open(&self, loc: Location) -> VfsResult<OpenResult> {
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
            OpenResult::Dir(loc)
        } else {
            // TODO(mivik): is this correct?
            let non_cacheable_type = matches!(
                loc.metadata()?.node_type,
                NodeType::CharacterDevice | NodeType::Fifo | NodeType::Socket
            );

            let direct = non_cacheable_type
                || self.path
                || self.direct
                || loc.flags().contains(NodeFlags::NON_CACHEABLE);
            let backend = if !direct || loc.flags().contains(NodeFlags::ALWAYS_CACHE) {
                FileBackend::new_cached(loc)?
            } else {
                FileBackend::new_direct(loc)
            };
            if self.truncate {
                backend.set_len(0)?;
            }
            OpenResult::File(File::new(backend, flags))
        })
    }

    /// Opens a file at the given [`Location`] using these options.
    #[cfg(feature = "vfs")]
    pub fn open_loc(&self, loc: Location) -> VfsResult<OpenResult> {
        if !self.is_valid() {
            return Err(VfsError::InvalidInput);
        }
        self._open(loc)
    }

    /// Opens a file at the given path relative to the provided [`FsContext`].
    pub fn open(&self, context: &FsContext, path: impl AsRef<Path>) -> VfsResult<OpenResult> {
        if !self.is_valid() {
            return Err(VfsError::InvalidInput);
        }

        // Empty pathname → NotFound. man "ENOENT — O_CREAT is not set and
        // the named file does not exist." resolve_parent("") would otherwise
        // return cwd itself which lets open() succeed — wrong per POSIX.
        // openat() does not accept AT_EMPTY_PATH; only specific *at calls do.
        // Fixes bug-openat-empty-path-no-enoent.
        if path.as_ref().as_str().is_empty() {
            return Err(VfsError::NotFound);
        }

        // Trailing-slash check: man — paths with trailing '/' must refer to
        // a directory. Components::parse_forward strips the empty trailing
        // component, so we use Path::has_trailing_slash() to recover the
        // signal. Captured early; the post-resolution check below enforces
        // it. Fixes bug-open-trailing-slash.
        let must_be_dir = path.as_ref().has_trailing_slash();

        let loc = match context.resolve_parent(path.as_ref()) {
            Ok((parent, name)) => {
                // If the path ends with '/', Linux never creates regular
                // files via O_CREAT here — the path explicitly requests a
                // directory, and open() cannot create directories. Suppress
                // create flags BEFORE open_file to avoid creating an inode
                // that the post-check would then reject (codex P1: original
                // ordering left a stale file on disk for failing calls).
                let effective_create = self.create && !must_be_dir;
                let effective_create_new = self.create_new && !must_be_dir;
                let mut loc = parent.open_file(
                    &name,
                    &axfs_ng_vfs::OpenOptions {
                        create: effective_create,
                        create_new: effective_create_new,
                        node_type: self.node_type,
                        permission: NodePermission::from_bits_truncate(self.mode as _),
                        user: self.user,
                    },
                )?;
                if !self.no_follow {
                    // Save the symlink-target path before resolving, so we can
                    // recurse into create-at-target if the target is dangling.
                    let was_symlink = loc.node_type() == NodeType::Symlink;
                    let symlink_target = if was_symlink && self.create {
                        loc.read_link().ok()
                    } else {
                        None
                    };
                    let parent_for_resolve = parent.clone();
                    match context
                        .with_current_dir(parent_for_resolve)?
                        .try_resolve_symlink(loc, &mut 0)
                    {
                        Ok(resolved) => loc = resolved,
                        Err(VfsError::NotFound) if self.create && symlink_target.is_some() => {
                            // O_CREAT on a dangling symlink: man — Linux follows
                            // the symlink and creates the target file (provided
                            // its parent directory exists). Recurse with the
                            // symlink target as the new path.
                            // Fixes bug-open-creat-dangling-no-create.
                            let target = symlink_target.unwrap();
                            return self.open(&context.with_current_dir(parent)?, &target);
                        }
                        Err(e) => return Err(e),
                    }
                } else if loc.node_type() == NodeType::Symlink && !self.path {
                    // O_NOFOLLOW + basename is a symlink + not O_PATH:
                    // man "If the trailing component (i.e., basename) of
                    // pathname is a symbolic link, then the open fails,
                    // with the error ELOOP."
                    //
                    // Precedence: a trailing slash on the original path
                    // forces the resolved entry to be a directory; a
                    // symlink itself is not a directory, so ENOTDIR
                    // takes priority over ELOOP (Linux behavior verified
                    // via host gcc: `open("/tmp/sym/", O_NOFOLLOW)` →
                    // ENOTDIR, not ELOOP). Without this check, starry
                    // returns ELOOP and diverges from Linux.
                    if must_be_dir {
                        return Err(VfsError::NotADirectory);
                    }
                    // Fixes bug-open-nofollow-sym.
                    return Err(VfsError::FilesystemLoop);
                }
                loc
            }
            Err(VfsError::InvalidInput) => {
                // root directory
                context.root_dir().clone()
            }
            Err(err) => return Err(err),
        };

        // Trailing-slash post-check: if the original pathname ended with '/'
        // (other than the root itself), the resolved location MUST be a
        // directory; otherwise return NotADirectory.
        if must_be_dir && !loc.is_dir() {
            return Err(VfsError::NotADirectory);
        }

        self._open(loc)
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
