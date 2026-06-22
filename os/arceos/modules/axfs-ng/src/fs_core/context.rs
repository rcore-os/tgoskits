use alloc::{
    borrow::{Cow, ToOwned},
    collections::vec_deque::VecDeque,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};

use ax_io::{Read, Write};
#[cfg(feature = "vfs")]
use axfs_ng_vfs::Mountpoint;
use axfs_ng_vfs::{
    Location, Metadata, NodePermission, NodeType, VfsError, VfsResult,
    path::{Component, Components, Path, PathBuf},
};
use spin::Once;

use crate::{
    file::File,
    os::sync::{IrqMutex, SleepMutex as Mutex},
};

/// Maximum number of symlinks that will be followed during path resolution.
pub const SYMLINKS_MAX: usize = 40;

/// Global root filesystem context, initialized once during [`init_filesystems`](crate::init_filesystems).
pub static ROOT_FS_CONTEXT: Once<FsContext> = Once::new();

/// Registry of all live `FsContext` instances (weak references).
///
/// Each time a task-local [`FS_CONTEXT`] is created, it registers its
/// `Arc<Mutex<FsContext>>` here via [`register_fs_context`].  This allows
/// [`FsContext::propagate_pivot_root`] to iterate over every task's
/// filesystem context and apply the same root / cwd fixup that Linux
/// performs in `chroot_fs_refs()` after `pivot_root(2)`.
static FS_REGISTRY: IrqMutex<Vec<Weak<Mutex<FsContext>>>> = IrqMutex::new(Vec::new());

/// Register an `FsContext` in the global [`FS_REGISTRY`].
fn register_fs_context(ctx: &Arc<Mutex<FsContext>>) {
    let mut registry = FS_REGISTRY.lock();
    // Prune dead weak references so the registry does not grow unboundedly
    // in long-running scenarios where pivot_root is never invoked.
    registry.retain(|weak| weak.upgrade().is_some());
    registry.push(Arc::downgrade(ctx));
}

/// Returns `true` if any live `FsContext` has its `root_dir` or `current_dir`
/// inside the given `mountpoint`.
#[cfg(feature = "vfs")]
pub fn is_mount_busy(mp: &Arc<Mountpoint>) -> bool {
    let refs: Vec<Arc<Mutex<FsContext>>> = {
        let mut registry = FS_REGISTRY.lock();
        registry.retain(|weak| weak.upgrade().is_some());
        registry.iter().filter_map(|weak| weak.upgrade()).collect()
    };
    for ctx_arc in refs {
        let ctx = ctx_arc.lock();
        if Arc::ptr_eq(ctx.root_dir().mountpoint(), mp)
            || Arc::ptr_eq(ctx.current_dir().mountpoint(), mp)
        {
            return true;
        }
    }
    false
}

scope_local::scope_local! {
    /// Task-local filesystem context, defaulting to a clone of [`ROOT_FS_CONTEXT`].
    pub static FS_CONTEXT: Arc<Mutex<FsContext>> = {
        let ctx = Arc::new(Mutex::new(
            ROOT_FS_CONTEXT
                .get()
                .expect("Root FS context not initialized")
                .clone(),
        ));
        register_fs_context(&ctx);
        ctx
    };
}

/// A single entry returned by [`FsContext::read_dir`].
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

/// Provides `std::fs`-like interface.
#[derive(Debug, Clone)]
pub struct FsContext {
    root_dir: Location,
    current_dir: Location,
}

impl FsContext {
    /// Creates a new context with `root_dir` as both root and current directory.
    pub fn new(root_dir: Location) -> Self {
        Self {
            root_dir: root_dir.clone(),
            current_dir: root_dir,
        }
    }

    /// Returns a reference to the root directory.
    pub fn root_dir(&self) -> &Location {
        &self.root_dir
    }

    /// Returns a reference to the current working directory.
    pub fn current_dir(&self) -> &Location {
        &self.current_dir
    }

    /// Changes the current working directory to `current_dir`.
    pub fn set_current_dir(&mut self, current_dir: Location) -> VfsResult<()> {
        current_dir.check_is_dir()?;
        self.current_dir = current_dir;
        Ok(())
    }

    /// Returns a new context that shares the same root but uses `current_dir` as
    /// the working directory.
    pub fn with_current_dir(&self, current_dir: Location) -> VfsResult<Self> {
        current_dir.check_is_dir()?;
        Ok(Self {
            root_dir: self.root_dir.clone(),
            current_dir,
        })
    }

    /// Attempts to resolve a possible symlink, at the current location (this
    /// assumes that `loc` is a child of current directory).
    pub fn try_resolve_symlink(
        &self,
        loc: Location,
        follow_count: &mut usize,
    ) -> VfsResult<Location> {
        if loc.node_type() != NodeType::Symlink {
            return Ok(loc);
        }
        if *follow_count >= SYMLINKS_MAX {
            return Err(VfsError::FilesystemLoop);
        }
        *follow_count += 1;
        let target = loc.read_link()?;
        if target.is_empty() {
            return Err(VfsError::NotFound);
        }
        self.resolve_components(PathBuf::from(target).components(), follow_count)
    }

    fn lookup(&self, dir: &Location, name: &str, follow_count: &mut usize) -> VfsResult<Location> {
        let loc = dir.lookup_no_follow(name)?;
        self.with_current_dir(dir.clone())?
            .try_resolve_symlink(loc, follow_count)
    }

    fn resolve_components(
        &self,
        components: Components,
        follow_count: &mut usize,
    ) -> VfsResult<Location> {
        let mut dir = self.current_dir.clone();
        for comp in components {
            match comp {
                Component::CurDir => {}
                Component::ParentDir => {
                    dir = dir.parent().unwrap_or_else(|| self.root_dir.clone());
                }
                Component::RootDir => {
                    dir = self.root_dir.clone();
                }
                Component::Normal(name) => {
                    dir = self.lookup(&dir, name, follow_count)?;
                }
            }
        }
        Ok(dir)
    }

    fn resolve_inner<'a>(
        &self,
        path: &'a Path,
        follow_count: &mut usize,
    ) -> VfsResult<(Location, Option<&'a str>)> {
        let entry_name = path.file_name();
        let mut components = path.components();
        if entry_name.is_some() {
            components.next_back();
        }
        let dir = self.resolve_components(components, follow_count)?;
        dir.check_is_dir()?;
        Ok((dir, entry_name))
    }

    /// Resolves a path starting from `current_dir`.
    pub fn resolve(&self, path: impl AsRef<Path>) -> VfsResult<Location> {
        let mut follow_count = 0;
        let (dir, name) = self.resolve_inner(path.as_ref(), &mut follow_count)?;
        match name {
            Some(name) => self.lookup(&dir, name, &mut follow_count),
            None => Ok(dir),
        }
    }

    /// Resolves a path starting from `current_dir` not following symlinks.
    pub fn resolve_no_follow(&self, path: impl AsRef<Path>) -> VfsResult<Location> {
        let (dir, name) = self.resolve_inner(path.as_ref(), &mut 0)?;
        match name {
            Some(name) => dir.lookup_no_follow(name),
            None => Ok(dir),
        }
    }

    /// Taking current node as root directory, resolves a path starting from
    /// `current_dir`.
    ///
    /// Returns `(parent_dir, entry_name)`, where `entry_name` is the name of
    /// the entry.
    pub fn resolve_parent<'a>(&self, path: &'a Path) -> VfsResult<(Location, Cow<'a, str>)> {
        let (dir, name) = self.resolve_inner(path, &mut 0)?;
        if let Some(name) = name {
            Ok((dir, Cow::Borrowed(name)))
        } else if let Some(parent) = dir.parent() {
            Ok((parent, dir.name().into_owned().into()))
        } else {
            Err(VfsError::InvalidInput)
        }
    }

    /// Resolves a path starting from `current_dir`, returning the parent
    /// directory and the name of the entry.
    ///
    /// This function requires that the entry does not exist and the parent
    /// exists. Note that, it does not perform an actual check to ensure the
    /// entry's non-existence. It simply raises an error if the entry name is
    /// not present in the path.
    pub fn resolve_nonexistent<'a>(&self, path: &'a Path) -> VfsResult<(Location, &'a str)> {
        let (dir, name) = self.resolve_inner(path, &mut 0)?;
        if let Some(name) = name {
            Ok((dir, name))
        } else {
            Err(VfsError::InvalidInput)
        }
    }

    /// Retrieves metadata for the file.
    pub fn metadata(&self, path: impl AsRef<Path>) -> VfsResult<Metadata> {
        self.resolve(path)?.metadata()
    }

    /// Reads the entire contents of a file into a bytes vector.
    pub fn read(&self, path: impl AsRef<Path>) -> VfsResult<Vec<u8>> {
        let mut buf = Vec::new();
        let file = File::open(self, path.as_ref())?;
        (&file).read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// Reads the entire contents of a file into a string.
    pub fn read_to_string(&self, path: impl AsRef<Path>) -> VfsResult<String> {
        String::from_utf8(self.read(path)?).map_err(|_| VfsError::InvalidData)
    }

    /// Writes a slice as the entire contents of a file.
    ///
    /// This function will create a file if it does not exist, and will entirely
    /// replace its contents if it does.
    pub fn write(&self, path: impl AsRef<Path>, buf: impl AsRef<[u8]>) -> VfsResult<()> {
        let file = File::create(self, path.as_ref())?;
        (&file).write_all(buf.as_ref())?;
        Ok(())
    }

    /// Returns an iterator over the entries in a directory.
    pub fn read_dir(&self, path: impl AsRef<Path>) -> VfsResult<ReadDir> {
        let dir = self.resolve(path)?;
        Ok(ReadDir {
            dir,
            buf: VecDeque::new(),
            offset: 0,
            ended: false,
        })
    }

    /// Removes a file from the filesystem.
    pub fn remove_file(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        let entry = self.resolve_no_follow(path.as_ref())?;
        entry
            .parent()
            .ok_or(VfsError::IsADirectory)?
            .unlink(&entry.name(), false)
    }

    /// Removes a directory from the filesystem.
    pub fn remove_dir(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        let entry = self.resolve_no_follow(path.as_ref())?;
        let dir = entry.entry().as_dir()?;
        if dir.has_children()? {
            return Err(VfsError::DirectoryNotEmpty);
        }
        entry
            .parent()
            .ok_or(VfsError::ResourceBusy)?
            .unlink(&entry.name(), true)
    }

    /// Renames a file or directory to a new name, replacing the original file
    /// if `to` already exists.
    pub fn rename(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) -> VfsResult<()> {
        let (src_dir, src_name) = self.resolve_parent(from.as_ref())?;
        let (dst_dir, dst_name) = self.resolve_parent(to.as_ref())?;
        src_dir.rename(&src_name, &dst_dir, &dst_name)
    }

    /// Creates a new, empty directory at the provided path.
    pub fn create_dir(
        &self,
        path: impl AsRef<Path>,
        mode: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<Location> {
        let path = path.as_ref();
        // Empty path should return NotFound, not InvalidInput
        if path.as_str().is_empty() {
            return Err(VfsError::NotFound);
        }
        let (dir, name) = match self.resolve_nonexistent(path) {
            Ok(pair) => pair,
            Err(VfsError::InvalidInput) => {
                // Path has no filename component (e.g. "/" or ".").
                // Resolve it: if it exists and is a directory, return
                // AlreadyExists (matching Linux EEXIST behaviour for mkdir("/")).
                return match self.resolve(path) {
                    Ok(loc) if loc.node_type() == NodeType::Directory => {
                        Err(VfsError::AlreadyExists)
                    }
                    Ok(_) => Err(VfsError::NotADirectory),
                    Err(e) => Err(e),
                };
            }
            Err(e) => return Err(e),
        };
        dir.create(name, NodeType::Directory, mode, uid, gid)
    }

    /// Creates a new hard link on the filesystem.
    pub fn link(
        &self,
        old_path: impl AsRef<Path>,
        new_path: impl AsRef<Path>,
    ) -> VfsResult<Location> {
        let old = self.resolve(old_path.as_ref())?;
        let (new_dir, new_name) = self.resolve_nonexistent(new_path.as_ref())?;
        new_dir.link(new_name, &old)
    }

    /// Creates a new symbolic link on the filesystem.
    pub fn symlink(
        &self,
        target: impl AsRef<str>,
        link_path: impl AsRef<Path>,
        uid: u32,
        gid: u32,
    ) -> VfsResult<Location> {
        let (dir, name) = self.resolve_nonexistent(link_path.as_ref())?;
        if dir.lookup_no_follow(name).is_ok() {
            return Err(VfsError::AlreadyExists);
        }
        let symlink = dir.create(name, NodeType::Symlink, NodePermission::default(), uid, gid)?;
        symlink.entry().as_file()?.set_symlink(target.as_ref())?;
        Ok(symlink)
    }

    /// Returns the canonical, absolute form of a path.
    pub fn canonicalize(&self, path: impl AsRef<Path>) -> VfsResult<PathBuf> {
        self.resolve(path.as_ref())?.absolute_path()
    }

    /// Pivot the root filesystem to `new_root`, moving the old root to
    /// `put_old` (which must be a directory under `new_root`).
    ///
    /// This follows Linux `pivot_root(2)` semantics: after the call the old
    /// root filesystem is accessible at `put_old`, and can be unmounted from
    /// there.
    ///
    /// Note: this method only updates **this** `FsContext`.  The caller must
    /// invoke [`FsContext::propagate_pivot_root`] afterwards to update every
    /// other task whose root / cwd still points at the old root, mirroring
    /// Linux's `chroot_fs_refs()`.
    pub fn pivot_root(&mut self, new_root: Location, put_old: Location) -> VfsResult<()> {
        let old_root = self.root_dir.clone();
        let old_root_mp = self.root_dir.mountpoint().clone();
        let new_root_mp = new_root.mountpoint().clone();
        old_root_mp.pivot_mount(&new_root_mp, &put_old)?;
        let new_root_loc = new_root_mp.root_location();
        self.root_dir = new_root_loc.clone();
        // Only replace cwd if it was pointing at the old root — mirrors
        // Linux's chroot_fs_refs / replace_path semantics.
        if old_root.ptr_eq(&self.current_dir) {
            self.current_dir = new_root_loc;
        }
        Ok(())
    }

    /// After a successful [`pivot_root`](Self::pivot_root), propagate the
    /// root / cwd change to **all** other tasks in the same mount namespace.
    ///
    /// This mirrors `chroot_fs_refs()` in Linux's `fs/namespace.c`:
    /// after `pivot_root(2)` reorganises the mount tree the kernel walks
    /// every thread's `fs_struct` and switches any `root` / `pwd` that
    /// pointed at the old root over to the new root.
    ///
    /// * `old_root` – the `Location` of the old root **before** the pivot
    ///   (obtained from `ctx.root_dir().clone()` before calling
    ///   [`pivot_root`](Self::pivot_root)).
    /// * `new_root` – the `Location` of the new root **after** the pivot
    ///   (obtained from `ctx.root_dir()` after calling
    ///   [`pivot_root`](Self::pivot_root)).
    ///
    /// # Linux semantics
    ///
    /// For each registered `FsContext`, [`Location::ptr_eq`] is used to
    /// compare both mountpoint **and** dentry, mirroring the kernel's
    /// `replace_path()` check `fs->root.mnt == old_root->mnt &&
    /// fs->root.dentry == old_root->dentry`:
    /// - If `root_dir` is exactly `old_root` → set to `new_root`.
    /// - If `current_dir` is exactly `old_root` → set to `new_root`.
    ///
    /// This avoids incorrectly updating tasks that have chroot'd into a
    /// subdirectory of the old root (same mountpoint, different dentry).
    pub fn propagate_pivot_root(old_root: &Location, new_root: &Location) {
        // 1. Collect strong references while holding the registry lock, then
        //    release it so we never nest two Mutex guards.
        let refs: Vec<Arc<Mutex<FsContext>>> = {
            let mut registry = FS_REGISTRY.lock();
            registry.retain(|weak| weak.upgrade().is_some());
            registry.iter().filter_map(|weak| weak.upgrade()).collect()
        };

        // 2. Walk every live FsContext and apply the same logic as
        //    Linux chroot_fs_refs().
        for ctx_arc in refs {
            let mut ctx = ctx_arc.lock();

            let update_root = old_root.ptr_eq(&ctx.root_dir);
            let update_cwd = old_root.ptr_eq(&ctx.current_dir);

            if update_root {
                ctx.root_dir = new_root.clone();
            }
            if update_cwd {
                ctx.current_dir = new_root.clone();
            }
        }
    }
}

/// Iterator returned by [`FsContext::read_dir`].
pub struct ReadDir {
    dir: Location,
    buf: VecDeque<ReadDirEntry>,
    offset: u64,
    ended: bool,
}

impl ReadDir {
    /// Maximum number of entries to buffer per `read_dir` syscall.
    // TODO: tune this
    pub const BUF_SIZE: usize = 128;
}

impl Iterator for ReadDir {
    type Item = VfsResult<ReadDirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }

        if self.buf.is_empty() {
            self.buf.clear();
            let result = self.dir.read_dir(
                self.offset,
                &mut |name: &str, ino: u64, node_type: NodeType, offset: u64| {
                    self.buf.push_back(ReadDirEntry {
                        name: name.to_owned(),
                        ino,
                        node_type,
                        offset,
                    });
                    self.offset = offset;
                    self.buf.len() < Self::BUF_SIZE
                },
            );

            // We handle errors only if we didn't get any entries
            if self.buf.is_empty() {
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
