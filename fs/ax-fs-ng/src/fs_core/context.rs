use alloc::{
    borrow::ToOwned, boxed::Box, collections::vec_deque::VecDeque, string::String, sync::Arc,
};

use ax_lazyinit::OnceLock;
use axfs_ng_vfs::{
    DirectoryCursor, DirectoryReadState, Location, MountNamespace, Mountpoint, NodeType,
    PathHandle, VfsError, VfsResult,
};

use super::{FsContextOwner, FsContextSnapshot};

/// Maximum number of symlinks that will be followed during path resolution.
pub const SYMLINKS_MAX: usize = 40;

/// The independent boot root owner; ordinary task scopes fork its snapshot.
pub static ROOT_FS_CONTEXT: OnceLock<FsContextOwner> = OnceLock::new();

scope_local::scope_local! {
    /// A real task share. None means the scope has retired its filesystem state.
    pub static FS_CONTEXT: Option<FsContextOwner> = Some(
        ROOT_FS_CONTEXT.get().expect("Root FS context not initialized").snapshot().fork_owner()
    );
}

/// Freezes paths and the exact namespace inside one short scope lease.
/// Snapshotting neither increments task shares nor acquires the topology gate.
pub fn current_fs_context() -> FsContextSnapshot {
    FS_CONTEXT.with(|owner| {
        owner
            .as_ref()
            .expect("filesystem context already released")
            .snapshot()
    })
}

/// A single entry returned by [`super::FsView::read_dir`].
pub struct ReadDirEntry {
    /// Entry name (file or directory name, not the full path).
    pub name: String,
    /// Inode number.
    pub ino: u64,
    /// Type of the node (file, directory, symlink, etc.).
    pub node_type: NodeType,
    /// Byte offset inside the directory (used for seeking).
    pub offset: u64,
}

/// Immutable path-resolution view. Task state changes belong to FsContextOwner.
#[derive(Debug, Clone)]
pub struct FsContext {
    namespace: alloc::sync::Arc<axfs_ng_vfs::MountNamespace>,
    root_dir: PathHandle,
    current_dir: PathHandle,
    /// The directory at which relative-path permission checks must stop.
    ///
    /// A context created for an `*at` call carries its directory fd here. This
    /// is deliberately separate from `root_dir`: the fd supplies a path-walk
    /// starting point, not a process root or a `..`-containment boundary.
    permission_root: Option<PathHandle>,
}

impl FsContext {
    /// Creates a native preparation view from an explicitly supplied new tree.
    /// Existing task trees must be accessed through their owner's snapshot.
    /// This may allocate, sleep, or release failed inputs; call outside gates.
    pub fn from_tree(tree: Arc<Mountpoint>, root: Location) -> VfsResult<Self> {
        root.check_is_dir()?;
        let root = PathHandle::acquire(root)?;
        let namespace = MountNamespace::from_tree(tree)?.into_namespace();
        Ok(Self::from_paths(namespace, root.clone(), root))
    }

    pub(super) fn from_paths(
        namespace: alloc::sync::Arc<axfs_ng_vfs::MountNamespace>,
        root_dir: PathHandle,
        current_dir: PathHandle,
    ) -> Self {
        Self {
            namespace,
            root_dir,
            current_dir,
            permission_root: None,
        }
    }

    /// Borrows the actual namespace associated with this immutable view.
    pub fn mount_namespace(&self) -> &alloc::sync::Arc<axfs_ng_vfs::MountNamespace> {
        &self.namespace
    }

    /// Borrows the selected root while its exact mount use remains owned.
    pub fn root_dir(&self) -> &Location {
        self.root_dir.location()
    }

    /// Borrows the selected cwd while its exact mount use remains owned.
    pub fn current_dir(&self) -> &Location {
        self.current_dir.location()
    }

    pub(super) fn root_handle(&self) -> &PathHandle {
        &self.root_dir
    }
    pub(super) fn cwd_handle(&self) -> &PathHandle {
        &self.current_dir
    }

    /// Selects an owned start without releasing and reacquiring its mount use.
    pub(super) fn with_directory_handle(mut self, directory: PathHandle) -> VfsResult<Self> {
        directory.location().check_is_dir()?;
        self.permission_root = Some(directory.clone());
        self.current_dir = directory;
        Ok(self)
    }

    /// Selects a different relative starting directory without changing task state.
    pub fn with_current_dir(&self, current_dir: PathHandle) -> VfsResult<Self> {
        current_dir.location().check_is_dir()?;
        let mut view = Self::from_paths(self.namespace.clone(), self.root_dir.clone(), current_dir);
        view.permission_root = self.permission_root.clone();
        Ok(view)
    }

    /// Returns the directory boundary used for relative-path permissions.
    pub fn permission_boundary(&self) -> Option<&PathHandle> {
        self.permission_root.as_ref()
    }
}

/// Iterator returned by [`super::FsView::read_dir`].
pub struct ReadDir {
    state: Box<dyn DirectoryReadState>,
    dir: PathHandle,
    buf: VecDeque<ReadDirEntry>,
    cursor: DirectoryCursor,
    ended: bool,
}

impl ReadDir {
    /// Maximum number of entries to buffer per `read_dir` syscall.
    // TODO: tune this
    pub const BUF_SIZE: usize = 128;

    pub(crate) fn from_path(dir: PathHandle) -> VfsResult<Self> {
        let state = dir.location().open_directory_read_state()?;
        Ok(Self {
            dir,
            state,
            buf: VecDeque::new(),
            cursor: DirectoryCursor::START,
            ended: false,
        })
    }
}

impl Iterator for ReadDir {
    type Item = VfsResult<ReadDirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }

        if self.buf.is_empty() {
            self.buf.clear();
            let mut invalid_name = false;
            let result = self.dir.location().read_dir_with_state(
                &mut *self.state,
                self.cursor,
                &mut |name: &[u8], ino: u64, node_type: NodeType, cursor: DirectoryCursor| {
                    if self.buf.len() == Self::BUF_SIZE {
                        return false;
                    }
                    let Ok(name) = core::str::from_utf8(name) else {
                        invalid_name = true;
                        return false;
                    };
                    self.buf.push_back(ReadDirEntry {
                        name: name.to_owned(),
                        ino,
                        node_type,
                        offset: cursor.offset(),
                    });
                    self.cursor = cursor;
                    true
                },
            );

            // We handle errors only if we didn't get any entries
            if self.buf.is_empty() {
                if invalid_name {
                    return Some(Err(VfsError::InvalidData));
                }
                if let Err(err) = result {
                    return Some(Err(err));
                }
                self.ended = true;
                return None;
            }
        }

        self.buf.pop_front().map(Ok)
    }
}
