#[cfg(feature = "vfs")]
use alloc::vec;
use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    collections::vec_deque::VecDeque,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
#[cfg(feature = "vfs")]
use core::sync::atomic::AtomicU64;
#[cfg(feature = "vfs")]
use core::sync::atomic::Ordering;

use ax_io::{Read, Write};
use ax_lazyinit::OnceLock;
#[cfg(feature = "vfs")]
use axfs_ng_vfs::Mountpoint;
use axfs_ng_vfs::{
    DirectoryCursor, DirectoryReadState, Location, Metadata, NodePermission, NodeType,
    RenameOptions, VfsError, VfsResult,
    path::{Component, Components, Path, PathBuf},
};

use crate::{
    file::File,
    os::sync::{IrqMutex, SleepMutex as Mutex},
};

type SearchCheck<'a> = Option<&'a dyn Fn(&Location) -> VfsResult<()>>;

/// Maximum number of symlinks that will be followed during path resolution.
pub const SYMLINKS_MAX: usize = 40;

/// Global root filesystem context, initialized once during [`init_filesystems`](crate::init_filesystems).
pub static ROOT_FS_CONTEXT: OnceLock<FsContext> = OnceLock::new();

/// Registry of all live `FsContext` instances (weak references).
///
/// Each time a task-local [`FS_CONTEXT`] is created, it registers its
/// `Arc<Mutex<FsContext>>` here via [`register_fs_context`]. This allows
/// [`FsContext::propagate_pivot_root`] to iterate over every task's
/// filesystem context and apply the same root / cwd fixup that Linux
/// performs in `chroot_fs_refs()` after `pivot_root(2)`.
static FS_REGISTRY: IrqMutex<Vec<Weak<Mutex<FsContext>>>> = IrqMutex::new(Vec::new());
#[cfg(feature = "vfs")]
static MOUNT_NAMESPACE_ID: AtomicU64 = AtomicU64::new(1);

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
        if !ctx.mount_namespace_contains(mp) {
            continue;
        }
        if Arc::ptr_eq(ctx.root_dir().mountpoint(), mp)
            || Arc::ptr_eq(ctx.current_dir().mountpoint(), mp)
        {
            return true;
        }
    }
    false
}

/// Namespace-local mount tree visible to an [`FsContext`].
#[cfg(feature = "vfs")]
#[derive(Debug, Clone)]
pub struct MountNamespace {
    id: u64,
    root_mount: Arc<Mountpoint>,
}

#[cfg(feature = "vfs")]
impl MountNamespace {
    fn new(root_mount: Arc<Mountpoint>) -> Self {
        Self {
            id: MOUNT_NAMESPACE_ID.fetch_add(1, Ordering::Relaxed),
            root_mount,
        }
    }

    /// Returns a kernel-local identifier for diagnostics.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Returns the root mountpoint of this namespace.
    pub fn root_mount(&self) -> &Arc<Mountpoint> {
        &self.root_mount
    }

    /// Walk the mount tree of this namespace, returning `(mount_id,
    /// parent_id, mountpoint)` tuples in DFS order.
    ///
    /// Delegates to [`Mountpoint::walk_tree`] on the root mount.
    pub fn walk_tree(&self) -> Vec<(u64, u64, Arc<Mountpoint>)> {
        self.root_mount.walk_tree()
    }

    fn clone_namespace(&self) -> Arc<Self> {
        Arc::new(Self::new(self.root_mount.clone_tree()))
    }

    fn contains_mountpoint(&self, mountpoint: &Arc<Mountpoint>) -> bool {
        let mut stack = vec![self.root_mount.clone()];
        while let Some(current) = stack.pop() {
            if Arc::ptr_eq(&current, mountpoint) {
                return true;
            }
            stack.extend(current.children());
        }
        false
    }
}

scope_local::scope_local! {
    /// The active task's filesystem owner. `None` means filesystem teardown
    /// completed; retained task objects must not keep cwd or mounts alive.
    pub static FS_CONTEXT: Option<Arc<Mutex<FsContext>>> = Some(
        ROOT_FS_CONTEXT
            .get()
            .expect("Root FS context not initialized")
            .clone()
            .into_shared()
    );
}

/// Returns an owned reference to the filesystem context of the active scope.
///
/// CPU pinning only covers the `Arc` clone. Callers may therefore acquire the
/// sleepable filesystem lock after preemption has been restored. This entry
/// must not be used after the task has released its filesystem owner on exit.
pub fn current_fs_context() -> Arc<Mutex<FsContext>> {
    FS_CONTEXT
        .clone_current()
        .expect("filesystem context already released")
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
    #[cfg(feature = "vfs")]
    mnt_ns: Arc<MountNamespace>,
    root_dir: Location,
    current_dir: Location,
}

impl FsContext {
    /// Publishes a shared context to mount-busy and pivot-root tracking.
    ///
    /// Every independently owned context, including an unshared replacement,
    /// must enter through this method. Clones of the returned Arc share the
    /// same registration; its weak entry expires after the last owner leaves.
    pub fn into_shared(self) -> Arc<Mutex<Self>> {
        let context = Arc::new(Mutex::new(self));
        register_fs_context(&context);
        context
    }

    /// Creates a new context with `root_dir` as both root and current directory.
    pub fn new(root_dir: Location) -> Self {
        #[cfg(feature = "vfs")]
        {
            let mnt_ns = Arc::new(MountNamespace::new(root_dir.mountpoint().clone()));
            Self::new_in_namespace(mnt_ns, root_dir)
        }
        #[cfg(not(feature = "vfs"))]
        {
            Self {
                root_dir: root_dir.clone(),
                current_dir: root_dir,
            }
        }
    }

    #[cfg(feature = "vfs")]
    fn new_in_namespace(mnt_ns: Arc<MountNamespace>, root_dir: Location) -> Self {
        Self {
            root_dir: root_dir.clone(),
            current_dir: root_dir,
            mnt_ns,
        }
    }

    /// Returns the mount namespace backing this filesystem context.
    #[cfg(feature = "vfs")]
    pub fn mount_namespace(&self) -> &Arc<MountNamespace> {
        &self.mnt_ns
    }

    #[cfg(feature = "vfs")]
    fn mount_namespace_contains(&self, mountpoint: &Arc<Mountpoint>) -> bool {
        self.mnt_ns.contains_mountpoint(mountpoint)
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
            #[cfg(feature = "vfs")]
            mnt_ns: self.mnt_ns.clone(),
        })
    }

    /// Rebind this context to a freshly cloned mount namespace.
    #[cfg(feature = "vfs")]
    pub fn unshare_mount_namespace(&mut self) -> VfsResult<()> {
        let new_ns = self.mnt_ns.clone_namespace();
        self.set_mount_namespace(new_ns)
    }

    /// Rebind this context to an existing mount namespace.
    #[cfg(feature = "vfs")]
    pub fn set_mount_namespace(&mut self, new_ns: Arc<MountNamespace>) -> VfsResult<()> {
        let root_path = self.root_dir.absolute_path()?;
        let current_path = self.current_dir.absolute_path()?;
        let new_root_loc = new_ns.root_mount().root_location();
        let resolver = Self::new_in_namespace(new_ns.clone(), new_root_loc);
        let root_dir = resolver.resolve(root_path)?;
        let current_dir = resolver.resolve(current_path)?;
        self.mnt_ns = new_ns;
        self.root_dir = root_dir;
        self.current_dir = current_dir;
        Ok(())
    }

    /// Attempts to resolve a possible symlink, at the current location (this
    /// assumes that `loc` is a child of current directory).
    pub fn try_resolve_symlink(
        &self,
        loc: Location,
        follow_count: &mut usize,
    ) -> VfsResult<Location> {
        self.try_resolve_symlink_using(loc, follow_count, None)
    }

    fn try_resolve_symlink_using(
        &self,
        loc: Location,
        follow_count: &mut usize,
        search: SearchCheck<'_>,
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
        let target = PathBuf::from(target);
        let resolved = self.resolve_components(target.components(), follow_count, search)?;
        Self::finish_checked_path(&target, resolved, search)
    }

    fn check_search(dir: &Location, search: SearchCheck<'_>) -> VfsResult<()> {
        if let Some(check) = search {
            dir.check_is_dir()?;
            check(dir)?;
        }
        Ok(())
    }

    fn ends_in_dot(path: &Path) -> bool {
        let path = path.as_str().trim_end_matches('/');
        path == "." || path.ends_with("/.")
    }

    fn finish_checked_path(
        path: &Path,
        resolved: Location,
        search: SearchCheck<'_>,
    ) -> VfsResult<Location> {
        if search.is_some() {
            // Components removes non-leading dots. Preserve the search that
            // an explicit final dot requires, including in symlink targets.
            if Self::ends_in_dot(path) {
                Self::check_search(&resolved, search)?;
            } else if path.as_str().ends_with('/') {
                resolved.check_is_dir()?;
            }
        }
        Ok(resolved)
    }

    fn lookup(
        &self,
        dir: &Location,
        name: &str,
        follow_count: &mut usize,
        search: SearchCheck<'_>,
    ) -> VfsResult<Location> {
        Self::check_search(dir, search)?;
        let loc = dir.lookup_no_follow(name)?;
        self.with_current_dir(dir.clone())?
            .try_resolve_symlink_using(loc, follow_count, search)
    }

    fn resolve_components(
        &self,
        components: Components,
        follow_count: &mut usize,
        search: SearchCheck<'_>,
    ) -> VfsResult<Location> {
        let mut dir = self.current_dir.clone();
        for comp in components {
            match comp {
                Component::CurDir => {}
                Component::ParentDir => {
                    Self::check_search(&dir, search)?;
                    if !dir.ptr_eq(&self.root_dir) {
                        dir = dir.parent().unwrap_or_else(|| self.root_dir.clone());
                    }
                }
                Component::RootDir => {
                    dir = self.root_dir.clone();
                }
                Component::Normal(name) => {
                    dir = self.lookup(&dir, name, follow_count, search)?;
                }
            }
        }
        Ok(dir)
    }

    fn resolve_inner<'a>(
        &self,
        path: &'a Path,
        follow_count: &mut usize,
        search: SearchCheck<'_>,
    ) -> VfsResult<(Location, Option<&'a str>)> {
        let entry_name = path.file_name();
        let mut components = path.components();
        if entry_name.is_some() {
            components.next_back();
        }
        let dir = self.resolve_components(components, follow_count, search)?;
        dir.check_is_dir()?;
        Ok((dir, entry_name))
    }

    fn resolve_using(
        &self,
        path: &Path,
        follow_final: bool,
        search: SearchCheck<'_>,
    ) -> VfsResult<Location> {
        let mut follow_count = 0;
        let (dir, name) = self.resolve_inner(path, &mut follow_count, search)?;
        let requires_directory =
            search.is_some() && (path.as_str().ends_with('/') || Self::ends_in_dot(path));
        let resolved = match name {
            Some(name) if follow_final || requires_directory => {
                self.lookup(&dir, name, &mut follow_count, search)?
            }
            Some(name) => {
                Self::check_search(&dir, search)?;
                dir.lookup_no_follow(name)?
            }
            None => dir,
        };
        Self::finish_checked_path(path, resolved, search)
    }

    /// Resolves a path starting from `current_dir`.
    pub fn resolve(&self, path: impl AsRef<Path>) -> VfsResult<Location> {
        self.resolve_using(path.as_ref(), true, None)
    }

    /// Resolves a path starting from `current_dir` not following symlinks.
    pub fn resolve_no_follow(&self, path: impl AsRef<Path>) -> VfsResult<Location> {
        self.resolve_using(path.as_ref(), false, None)
    }

    /// Resolves a path, checking each searched directory before traversal.
    /// The check also applies inside symbolic-link targets and before `..`.
    pub fn resolve_checked(
        &self,
        path: impl AsRef<Path>,
        check_search: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<Location> {
        self.resolve_using(path.as_ref(), true, Some(&check_search))
    }

    /// Resolves with directory search checks, without following the final link.
    /// A trailing slash or dot still requires traversal into a directory.
    pub fn resolve_no_follow_checked(
        &self,
        path: impl AsRef<Path>,
        check_search: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<Location> {
        self.resolve_using(path.as_ref(), false, Some(&check_search))
    }

    /// Resolves a relative path's parent without following intermediate
    /// symbolic links or allowing the walk to escape above `current_dir`.
    ///
    /// This provides the path-walk guarantees required by Linux openat2's
    /// `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` combination.
    pub fn resolve_parent_beneath_no_symlinks<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(Location, Cow<'a, str>)> {
        let mut components = path.components().peekable();
        let mut dir = self.current_dir.clone();
        let mut depth = 0usize;

        while let Some(component) = components.next() {
            let is_last = components.peek().is_none();
            match component {
                Component::RootDir => return Err(VfsError::CrossesDevices),
                Component::CurDir if is_last => {
                    if let Some(parent) = dir.parent() {
                        return Ok((parent, dir.name().into_owned().into()));
                    }
                    return Ok((dir, Cow::Borrowed(".")));
                }
                Component::CurDir => {}
                Component::ParentDir => {
                    if depth == 0 {
                        return Err(VfsError::CrossesDevices);
                    }
                    dir = dir.parent().ok_or(VfsError::CrossesDevices)?;
                    depth -= 1;
                    if is_last {
                        if let Some(parent) = dir.parent() {
                            return Ok((parent, dir.name().into_owned().into()));
                        }
                        return Ok((dir, Cow::Borrowed(".")));
                    }
                }
                Component::Normal(name) if is_last => return Ok((dir, Cow::Borrowed(name))),
                Component::Normal(name) => {
                    let next = dir.lookup_no_follow(name)?;
                    if next.node_type() == NodeType::Symlink {
                        return Err(VfsError::FilesystemLoop);
                    }
                    next.check_is_dir()?;
                    dir = next;
                    depth += 1;
                }
            }
        }

        Err(VfsError::NotFound)
    }

    /// Taking current node as root directory, resolves a path starting from
    /// `current_dir`.
    ///
    /// Returns `(parent_dir, entry_name)`, where `entry_name` is the name of
    /// the entry.
    pub fn resolve_parent<'a>(&self, path: &'a Path) -> VfsResult<(Location, Cow<'a, str>)> {
        let (dir, name) = self.resolve_inner(path, &mut 0, None)?;
        if let Some(name) = name {
            Ok((dir, Cow::Borrowed(name)))
        } else {
            if dir.ptr_eq(&self.root_dir) {
                Err(VfsError::InvalidInput)
            } else if let Some(parent) = dir.parent() {
                Ok((parent, dir.name().into_owned().into()))
            } else {
                Err(VfsError::InvalidInput)
            }
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
        let (dir, name) = self.resolve_inner(path, &mut 0, None)?;
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
        (&file)
            .read_to_end(&mut buf)
            .map_err(crate::io_error_to_vfs_error)?;
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
        (&file)
            .write_all(buf.as_ref())
            .map_err(crate::io_error_to_vfs_error)?;
        Ok(())
    }

    /// Returns an iterator over the entries in a directory.
    pub fn read_dir(&self, path: impl AsRef<Path>) -> VfsResult<ReadDir> {
        let dir = self.resolve(path)?;
        let state = dir.open_directory_read_state()?;
        Ok(ReadDir {
            dir,
            state,
            buf: VecDeque::new(),
            cursor: DirectoryCursor::START,
            ended: false,
        })
    }

    /// Removes a file from the filesystem.
    pub fn remove_file(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        let entry = self.resolve_no_follow(path.as_ref())?;
        if entry.ptr_eq(&self.root_dir) {
            return Err(VfsError::IsADirectory);
        }
        entry
            .parent()
            .ok_or(VfsError::IsADirectory)?
            .unlink(&entry.name(), false)
    }

    /// Removes a directory from the filesystem.
    pub fn remove_dir(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        let path = path.as_ref();
        // Path components normalize away trailing dots. Linux classifies the
        // final component before normalization, after resolving its parent.
        let trimmed = path.as_str().trim_end_matches('/');
        let last = trimmed.rsplit('/').next().unwrap_or("");
        if matches!(last, "." | "..") {
            let parent =
                trimmed.rsplit_once('/').map_or(
                    ".",
                    |(parent, _)| if parent.is_empty() { "/" } else { parent },
                );
            self.resolve(parent)?.check_is_dir()?;
            return Err(if last == "." {
                VfsError::InvalidInput
            } else {
                VfsError::DirectoryNotEmpty
            });
        }

        let entry = self.resolve_no_follow(path)?;
        if entry.ptr_eq(&self.root_dir) || entry.is_root_of_mount() {
            return Err(VfsError::ResourceBusy);
        }
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
        self.rename_with_options(from, to, RenameOptions::REPLACE)
    }

    /// Renames a path with typed `renameat2` behavior.
    pub fn rename_with_options(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
        options: RenameOptions,
    ) -> VfsResult<()> {
        let (src_dir, src_name) = self.resolve_parent(from.as_ref())?;
        let (dst_dir, dst_name) = self.resolve_parent(to.as_ref())?;
        src_dir.rename_with_options(&src_name, &dst_dir, &dst_name, options)
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
        if path.as_str().is_empty() {
            return Err(VfsError::NotFound);
        }
        // Check through the visible mount tree before asking the parent
        // filesystem to create the entry. A static pseudo-filesystem may
        // reject mutations with EPERM even though the mounted or generated
        // destination already exists; mkdir(2) must report EEXIST instead.
        if self.resolve_no_follow(path).is_ok() {
            return Err(VfsError::AlreadyExists);
        }
        let (dir, name) = self.resolve_nonexistent(path)?;
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
        dir.create_symlink(name, target.as_ref(), NodePermission::default(), uid, gid)
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
    pub fn propagate_pivot_root(
        #[cfg(feature = "vfs")] mount_namespace: &Arc<MountNamespace>,
        old_root: &Location,
        new_root: &Location,
    ) {
        // 1. Collect strong references while holding the registry lock, then
        //    release it so we never nest two PI mutex guards.
        let refs: Vec<Arc<Mutex<FsContext>>> = {
            let mut registry = FS_REGISTRY.lock();
            registry.retain(|weak| weak.upgrade().is_some());
            registry.iter().filter_map(|weak| weak.upgrade()).collect()
        };

        // 2. Walk every live FsContext and apply the same logic as
        //    Linux chroot_fs_refs().
        for ctx_arc in refs {
            let mut ctx = ctx_arc.lock();
            #[cfg(feature = "vfs")]
            if !Arc::ptr_eq(ctx.mount_namespace(), mount_namespace) {
                continue;
            }

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
    state: Box<dyn DirectoryReadState>,
    buf: VecDeque<ReadDirEntry>,
    cursor: DirectoryCursor,
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
            let mut invalid_name = false;
            let result = self.dir.read_dir_with_state(
                &mut *self.state,
                self.cursor,
                &mut |name: &[u8], ino: u64, node_type: NodeType, cursor: DirectoryCursor| {
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
                    self.buf.len() < Self::BUF_SIZE
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
