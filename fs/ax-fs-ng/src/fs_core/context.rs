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
    DirectoryCursor, DirectoryReadState, Location, Metadata, MutationCredentials, NodePermission,
    NodeType, RenameOptions, VfsError, VfsResult,
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
    /// The directory at which relative-path permission checks must stop.
    ///
    /// A context created for an `*at` call carries its directory fd here. This
    /// is deliberately separate from `root_dir`: the fd supplies a path-walk
    /// starting point, not a process root or a `..`-containment boundary.
    permission_root: Option<Location>,
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
                permission_root: None,
            }
        }
    }

    #[cfg(feature = "vfs")]
    fn new_in_namespace(mnt_ns: Arc<MountNamespace>, root_dir: Location) -> Self {
        Self {
            root_dir: root_dir.clone(),
            current_dir: root_dir,
            permission_root: None,
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
            permission_root: self.permission_root.clone(),
            #[cfg(feature = "vfs")]
            mnt_ns: self.mnt_ns.clone(),
        })
    }

    /// Returns a context whose relative paths and permission checks start at
    /// an already opened directory.
    pub fn with_dirfd(&self, dirfd: Location) -> VfsResult<Self> {
        let mut context = self.with_current_dir(dirfd.clone())?;
        context.permission_root = Some(dirfd);
        Ok(context)
    }

    /// Returns the directory boundary used for relative-path permissions.
    pub fn permission_boundary(&self) -> Option<&Location> {
        self.permission_root.as_ref()
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
        let permission_root_path = self
            .permission_root
            .as_ref()
            .map(Location::absolute_path)
            .transpose()?;
        let new_root_loc = new_ns.root_mount().root_location();
        let resolver = Self::new_in_namespace(new_ns.clone(), new_root_loc);
        let root_dir = resolver.resolve(root_path)?;
        let current_dir = resolver.resolve(current_path)?;
        let permission_root = permission_root_path
            .map(|path| resolver.resolve(path))
            .transpose()?;
        self.mnt_ns = new_ns;
        self.root_dir = root_dir;
        self.current_dir = current_dir;
        self.permission_root = permission_root;
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

    /// Resolves a symlink while checking every directory reached by its
    /// target before lookup.
    pub fn try_resolve_symlink_checked(
        &self,
        loc: Location,
        follow_count: &mut usize,
        check_search: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<Location> {
        self.try_resolve_symlink_using(loc, follow_count, Some(&check_search))
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

    fn try_resolve_symlink_with_trace(
        &self,
        loc: Location,
        follow_count: &mut usize,
        searched: &mut Vec<Location>,
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
        let resolved = self.resolve_components_with_trace(
            target.components(),
            follow_count,
            searched,
            search,
        )?;
        Self::finish_checked_path(&target, resolved, search)
    }

    fn lookup_with_trace(
        &self,
        dir: &Location,
        name: &str,
        follow_count: &mut usize,
        searched: &mut Vec<Location>,
        search: SearchCheck<'_>,
    ) -> VfsResult<Location> {
        searched.push(dir.clone());
        Self::check_search(dir, search)?;
        let loc = dir.lookup_no_follow(name)?;
        self.with_current_dir(dir.clone())?
            .try_resolve_symlink_with_trace(loc, follow_count, searched, search)
    }

    fn resolve_components_with_trace(
        &self,
        components: Components,
        follow_count: &mut usize,
        searched: &mut Vec<Location>,
        search: SearchCheck<'_>,
    ) -> VfsResult<Location> {
        let mut dir = self.current_dir.clone();
        for comp in components {
            match comp {
                Component::CurDir => {}
                Component::ParentDir => {
                    searched.push(dir.clone());
                    Self::check_search(&dir, search)?;
                    if !dir.ptr_eq(&self.root_dir) {
                        dir = dir.parent().unwrap_or_else(|| self.root_dir.clone());
                    }
                }
                Component::RootDir => {
                    dir = self.root_dir.clone();
                }
                Component::Normal(name) => {
                    dir = self.lookup_with_trace(&dir, name, follow_count, searched, search)?;
                }
            }
        }
        Ok(dir)
    }

    fn resolve_inner_with_trace<'a>(
        &self,
        path: &'a Path,
        follow_count: &mut usize,
        searched: &mut Vec<Location>,
        search: SearchCheck<'_>,
    ) -> VfsResult<(Location, Option<&'a str>)> {
        let entry_name = path.file_name();
        let mut components = path.components();
        if entry_name.is_some() {
            components.next_back();
        }
        let dir = self.resolve_components_with_trace(components, follow_count, searched, search)?;
        dir.check_is_dir()?;
        searched.push(dir.clone());
        Self::check_search(&dir, search)?;
        Ok((dir, entry_name))
    }

    /// Resolves a path and records every directory visited during traversal.
    pub fn resolve_with_search(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<(Location, Vec<Location>)> {
        let mut searched = Vec::new();
        let mut follow_count = 0;
        let (dir, name) =
            self.resolve_inner_with_trace(path.as_ref(), &mut follow_count, &mut searched, None)?;
        let location = match name {
            Some(name) => {
                self.lookup_with_trace(&dir, name, &mut follow_count, &mut searched, None)?
            }
            None => dir,
        };
        Ok((location, searched))
    }

    /// Resolves a path without following its final symlink and records the
    /// directories visited during traversal.
    pub fn resolve_no_follow_with_search(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<(Location, Vec<Location>)> {
        let mut searched = Vec::new();
        let (dir, name) =
            self.resolve_inner_with_trace(path.as_ref(), &mut 0, &mut searched, None)?;
        let location = match name {
            Some(name) => dir.lookup_no_follow(name)?,
            None => dir,
        };
        Ok((location, searched))
    }

    /// Resolves a path's parent and records every directory visited during
    /// traversal, including directories reached through symlink targets.
    pub fn resolve_parent_with_search<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(Location, Cow<'a, str>, Vec<Location>)> {
        self.resolve_parent_with_search_checked(path, |_| Ok(()))
    }

    /// Resolves a path's parent while checking each directory immediately
    /// before it is searched. The trace is still returned for callers that
    /// must enforce a separate dirfd boundary after resolution.
    pub fn resolve_parent_with_search_checked<'a>(
        &self,
        path: &'a Path,
        check_search: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<(Location, Cow<'a, str>, Vec<Location>)> {
        // An empty path is not the current directory. Keep the special
        // `.`/`..` handling below, but match Linux's ENOENT for mutations
        // given an empty pathname.
        if path.as_str().is_empty() {
            return Err(VfsError::NotFound);
        }
        let mut searched = Vec::new();
        let (dir, name) =
            self.resolve_inner_with_trace(path, &mut 0, &mut searched, Some(&check_search))?;
        if let Some(name) = name {
            Ok((dir, Cow::Borrowed(name), searched))
        } else if dir.ptr_eq(&self.root_dir) {
            Err(VfsError::InvalidInput)
        } else if let Some(parent) = dir.parent() {
            // `parent` is only used to represent the resolved final `.`/`..`
            // entry as `(parent, name)`. It was not traversed by the path
            // walk, so it must not be added to the search trace. This is
            // essential for an already-open dirfd whose own parent is not
            // searchable.
            Ok((parent, dir.name().into_owned().into(), searched))
        } else {
            Err(VfsError::InvalidInput)
        }
    }

    /// Resolves a path and records every searched directory while checking
    /// each one before lookup, including directories reached through
    /// intermediate symlinks.
    pub fn resolve_with_search_checked(
        &self,
        path: impl AsRef<Path>,
        check_search: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<(Location, Vec<Location>)> {
        let path = path.as_ref();
        let mut searched = Vec::new();
        let mut follow_count = 0;
        let (dir, name) = self.resolve_inner_with_trace(
            path,
            &mut follow_count,
            &mut searched,
            Some(&check_search),
        )?;
        let location = match name {
            Some(name) => self.lookup_with_trace(
                &dir,
                name,
                &mut follow_count,
                &mut searched,
                Some(&check_search),
            )?,
            None => dir,
        };
        let location = Self::finish_checked_path(path, location, Some(&check_search))?;
        Ok((location, searched))
    }

    /// Resolves a path without following its final symlink, checking each
    /// directory before lookup and retaining the search trace.
    pub fn resolve_no_follow_with_search_checked(
        &self,
        path: impl AsRef<Path>,
        check_search: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<(Location, Vec<Location>)> {
        let path = path.as_ref();
        let mut searched = Vec::new();
        let mut follow_count = 0;
        let (dir, name) = self.resolve_inner_with_trace(
            path,
            &mut follow_count,
            &mut searched,
            Some(&check_search),
        )?;
        let requires_directory = Self::ends_in_dot(path) || path.as_str().ends_with('/');
        let location = match name {
            Some(name) if requires_directory => self.lookup_with_trace(
                &dir,
                name,
                &mut follow_count,
                &mut searched,
                Some(&check_search),
            )?,
            Some(name) => {
                Self::check_search(&dir, Some(&check_search))?;
                dir.lookup_no_follow(name)?
            }
            None => dir,
        };
        let location = Self::finish_checked_path(path, location, Some(&check_search))?;
        Ok((location, searched))
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
        self.resolve_parent_beneath_no_symlinks_checked(path, |_| Ok(()))
    }

    /// Resolves the parent for `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` while
    /// checking search permission before every directory lookup. The
    /// no-symlink and beneath guarantees are identical to the unchecked
    /// variant; the callback only supplies the caller's DAC policy.
    pub fn resolve_parent_beneath_no_symlinks_checked<'a>(
        &self,
        path: &'a Path,
        check_search: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<(Location, Cow<'a, str>)> {
        let mut components = path.components().peekable();
        let mut dir = self.current_dir.clone();
        let mut depth = 0usize;

        while let Some(component) = components.next() {
            let is_last = components.peek().is_none();
            match component {
                Component::RootDir => return Err(VfsError::CrossesDevices),
                Component::CurDir if is_last => {
                    check_search(&dir)?;
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
                    check_search(&dir)?;
                    dir = dir.parent().ok_or(VfsError::CrossesDevices)?;
                    depth -= 1;
                    if is_last {
                        if let Some(parent) = dir.parent() {
                            return Ok((parent, dir.name().into_owned().into()));
                        }
                        return Ok((dir, Cow::Borrowed(".")));
                    }
                }
                Component::Normal(name) if is_last => {
                    check_search(&dir)?;
                    return Ok((dir, Cow::Borrowed(name)));
                }
                Component::Normal(name) => {
                    check_search(&dir)?;
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

    /// Check one DAC permission class for a filesystem location.
    fn check_permission(
        location: &Location,
        credentials: &MutationCredentials<'_>,
        required: NodePermission,
    ) -> VfsResult<()> {
        if credentials.cap_dac_override {
            return Ok(());
        }

        // CAP_DAC_READ_SEARCH bypasses read/search checks, but it does not
        // grant write access to a directory. Keep write bits in the normal
        // DAC path even when a read/search capability is present.
        const READ_SEARCH_PERMISSIONS: NodePermission = NodePermission::OWNER_READ
            .union(NodePermission::GROUP_READ)
            .union(NodePermission::OTHER_READ)
            .union(NodePermission::OWNER_EXEC)
            .union(NodePermission::GROUP_EXEC)
            .union(NodePermission::OTHER_EXEC);
        if credentials.cap_dac_read_search
            && required.difference(READ_SEARCH_PERMISSIONS).is_empty()
        {
            return Ok(());
        }

        let metadata = location.metadata()?;
        let mode = if credentials.fsuid == metadata.uid {
            metadata.mode.bits() >> 6
        } else if credentials.in_group(metadata.gid) {
            metadata.mode.bits() >> 3
        } else {
            metadata.mode.bits()
        };
        if NodePermission::from_bits_truncate(mode).contains(required) {
            Ok(())
        } else {
            Err(VfsError::PermissionDenied)
        }
    }

    /// Check execute/search permission up to the selected path boundary.
    pub fn check_search_path(
        &self,
        directory: &Location,
        boundary: Option<&Location>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        let mut current = directory.clone();
        loop {
            Self::check_permission(&current, credentials, NodePermission::OTHER_EXEC)?;
            if boundary.is_some_and(|boundary| current.ptr_eq(boundary))
                || current.ptr_eq(&self.root_dir)
            {
                return Ok(());
            }
            current = current.parent().ok_or(VfsError::InvalidInput)?;
        }
    }

    fn check_mutation_parent_with_boundary(
        &self,
        directory: &Location,
        boundary: Option<&Location>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        self.check_search_path(directory, boundary, credentials)?;
        Self::check_permission(directory, credentials, NodePermission::OTHER_WRITE)
    }

    /// Check search permission for every directory visited by path lookup.
    pub(crate) fn check_search_trace(
        &self,
        searched: &[Location],
        boundary: Option<&Location>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        for directory in searched {
            self.check_search_path(directory, boundary, credentials)?;
        }
        Ok(())
    }

    fn check_mutation_parent_with_boundary_and_search(
        &self,
        directory: &Location,
        searched: &[Location],
        boundary: Option<&Location>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        self.check_search_trace(searched, boundary, credentials)?;
        self.check_mutation_parent_with_boundary(directory, boundary, credentials)
    }

    /// Check a mutation parent and every directory traversed to reach it.
    pub(crate) fn check_mutation_parent_with_search(
        &self,
        directory: &Location,
        searched: &[Location],
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        self.check_mutation_parent_with_boundary_and_search(
            directory,
            searched,
            self.permission_root.as_ref(),
            credentials,
        )
    }

    /// Check sticky-directory ownership rules for removing or replacing an entry.
    fn check_sticky(
        directory: &Location,
        target: &Location,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        let directory_metadata = directory.metadata()?;
        if !directory_metadata.mode.contains(NodePermission::STICKY) {
            return Ok(());
        }

        let target_metadata = target.metadata()?;
        if credentials.cap_fowner
            || credentials.fsuid == directory_metadata.uid
            || credentials.fsuid == target_metadata.uid
        {
            Ok(())
        } else {
            Err(VfsError::OperationNotPermitted)
        }
    }

    /// Removes a file from the filesystem.
    pub fn remove_file(
        &self,
        path: impl AsRef<Path>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        let (entry, searched) =
            self.resolve_no_follow_with_search_checked(path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_root.as_ref(), credentials)
            })?;
        if entry.ptr_eq(&self.root_dir) {
            return Err(VfsError::IsADirectory);
        }
        let directory = entry.parent().ok_or(VfsError::IsADirectory)?;
        self.check_mutation_parent_with_search(&directory, &searched, credentials)?;
        Self::check_sticky(&directory, &entry, credentials)?;
        directory.unlink(&entry.name(), false)
    }

    /// Removes a directory from the filesystem.
    pub fn remove_dir(
        &self,
        path: impl AsRef<Path>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
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

        let (entry, searched) = self.resolve_no_follow_with_search_checked(path, |directory| {
            self.check_search_path(directory, self.permission_root.as_ref(), credentials)
        })?;
        if entry.ptr_eq(&self.root_dir) || entry.is_root_of_mount() {
            return Err(VfsError::ResourceBusy);
        }
        let directory = entry.parent().ok_or(VfsError::ResourceBusy)?;
        self.check_mutation_parent_with_search(&directory, &searched, credentials)?;
        Self::check_sticky(&directory, &entry, credentials)?;
        let dir = entry.entry().as_dir()?;
        if dir.has_children()? {
            return Err(VfsError::DirectoryNotEmpty);
        }
        directory.unlink(&entry.name(), true)
    }

    /// Renames a file or directory to a new name, replacing the original file
    /// if `to` already exists.
    pub fn rename(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        self.rename_with_options(from, to, RenameOptions::REPLACE, credentials)
    }

    /// Renames a path with typed `renameat2` behavior.
    pub fn rename_with_options(
        &self,
        from: impl AsRef<Path>,
        to: impl AsRef<Path>,
        options: RenameOptions,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        let (src_dir, src_name, src_searched) =
            self.resolve_parent_with_search_checked(from.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_root.as_ref(), credentials)
            })?;
        let (dst_dir, dst_name, dst_searched) =
            self.resolve_parent_with_search_checked(to.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_root.as_ref(), credentials)
            })?;
        self.rename_locations_with_boundaries_and_search(
            (&src_dir, &src_name),
            (&dst_dir, &dst_name),
            options,
            (self.permission_root.as_ref(), self.permission_root.as_ref()),
            (&src_searched, &dst_searched),
            credentials,
        )
    }

    /// Rename already resolved entries after applying directory authorization.
    pub fn rename_locations(
        &self,
        src_dir: &Location,
        src_name: &str,
        dst_dir: &Location,
        dst_name: &str,
        options: RenameOptions,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        self.rename_locations_with_boundaries(
            (src_dir, src_name),
            (dst_dir, dst_name),
            options,
            (self.permission_root.as_ref(), self.permission_root.as_ref()),
            credentials,
        )
    }

    /// Rename resolved entries with independent source and destination path
    /// permission boundaries.
    pub fn rename_locations_with_boundaries(
        &self,
        source: (&Location, &str),
        destination: (&Location, &str),
        options: RenameOptions,
        boundaries: (Option<&Location>, Option<&Location>),
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        let (src_dir, src_name) = source;
        let (dst_dir, dst_name) = destination;
        self.rename_locations_with_boundaries_and_search(
            (src_dir, src_name),
            (dst_dir, dst_name),
            options,
            boundaries,
            (&[], &[]),
            credentials,
        )
    }

    pub fn rename_locations_with_boundaries_and_search(
        &self,
        source: (&Location, &str),
        destination: (&Location, &str),
        options: RenameOptions,
        boundaries: (Option<&Location>, Option<&Location>),
        searches: (&[Location], &[Location]),
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        let (src_dir, src_name) = source;
        let (dst_dir, dst_name) = destination;
        let (src_boundary, dst_boundary) = boundaries;
        let (src_search, dst_search) = searches;
        // Search permission is a prerequisite for looking up either final
        // entry. Keep this inside the VFS operation so callers cannot observe
        // ENOENT/EEXIST from an inaccessible parent before DAC search checks.
        self.check_search_trace(src_search, src_boundary, credentials)?;
        self.check_search_trace(dst_search, dst_boundary, credentials)?;
        let source = src_dir.lookup_no_follow(src_name)?;
        let destination = match dst_dir.lookup_no_follow(dst_name) {
            Ok(destination) => Some(destination),
            Err(VfsError::NotFound) => None,
            Err(error) => return Err(error),
        };

        // Resolve both final entries before checking parent write access. A
        // missing source must remain ENOENT even when either mutation parent
        // is searchable but not writable.
        self.check_mutation_parent_with_boundary_and_search(
            src_dir,
            src_search,
            src_boundary,
            credentials,
        )?;
        self.check_mutation_parent_with_boundary_and_search(
            dst_dir,
            dst_search,
            dst_boundary,
            credentials,
        )?;

        if options.no_replace() && destination.is_some() {
            return Err(VfsError::AlreadyExists);
        }

        // Match the VFS no-op result before applying sticky-directory removal
        // rules to an unchanged ordinary rename.
        if options == RenameOptions::REPLACE
            && Arc::ptr_eq(src_dir.mountpoint(), dst_dir.mountpoint())
            && destination
                .as_ref()
                .is_some_and(|destination| destination.inode() == source.inode())
        {
            return Ok(());
        }

        Self::check_sticky(src_dir, &source, credentials)?;
        if !options.no_replace()
            && let Some(destination) = &destination
        {
            Self::check_sticky(dst_dir, destination, credentials)?;
        }
        src_dir.rename_with_options(src_name, dst_dir, dst_name, options)
    }

    /// Creates a new, empty directory at the provided path.
    pub fn create_dir(
        &self,
        path: impl AsRef<Path>,
        mode: NodePermission,
        uid: u32,
        gid: u32,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<Location> {
        let path = path.as_ref();
        if path.as_str().is_empty() {
            return Err(VfsError::NotFound);
        }
        let (dir, name, searched) = self.resolve_parent_with_search_checked(path, |directory| {
            self.check_search_path(directory, self.permission_root.as_ref(), credentials)
        })?;
        match dir.lookup_no_follow(&name) {
            Ok(_) => return Err(VfsError::AlreadyExists),
            Err(VfsError::NotFound) => {}
            Err(error) => return Err(error),
        }
        self.check_mutation_parent_with_search(&dir, &searched, credentials)?;
        dir.create(&name, NodeType::Directory, mode, uid, gid)
    }

    /// Creates a new hard link on the filesystem.
    pub fn link(
        &self,
        old_path: impl AsRef<Path>,
        new_path: impl AsRef<Path>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<Location> {
        let (old, old_searched) =
            self.resolve_with_search_checked(old_path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_root.as_ref(), credentials)
            })?;
        let (new_dir, new_name, new_searched) =
            self.resolve_parent_with_search_checked(new_path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_root.as_ref(), credentials)
            })?;
        self.link_locations_with_boundaries_and_search(
            (&old, &old_searched),
            (&new_dir, &new_name, &new_searched),
            (self.permission_root.as_ref(), self.permission_root.as_ref()),
            credentials,
        )
    }

    /// Create a hard link between already resolved locations.
    pub fn link_locations(
        &self,
        old: &Location,
        new_dir: &Location,
        new_name: &str,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<Location> {
        self.link_locations_with_boundaries_and_search(
            (old, &[]),
            (new_dir, new_name, &[]),
            (self.permission_root.as_ref(), self.permission_root.as_ref()),
            credentials,
        )
    }

    /// Create a hard link with independent source and destination path
    /// permission boundaries.
    pub fn link_locations_with_boundaries(
        &self,
        old: &Location,
        new_dir: &Location,
        new_name: &str,
        old_boundary: Option<&Location>,
        new_boundary: Option<&Location>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<Location> {
        self.link_locations_with_boundaries_and_search(
            (old, &[]),
            (new_dir, new_name, &[]),
            (old_boundary, new_boundary),
            credentials,
        )
    }

    pub fn link_locations_with_boundaries_and_search(
        &self,
        source: (&Location, &[Location]),
        destination: (&Location, &str, &[Location]),
        boundaries: (Option<&Location>, Option<&Location>),
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<Location> {
        let (old, old_search) = source;
        let (new_dir, new_name, new_search) = destination;
        let (old_boundary, new_boundary) = boundaries;
        // A directory cannot be hard-linked. Check this before inspecting
        // the source parent: `linkat(dirfd, ".", ...)` must not re-walk a
        // parent outside the dirfd permission boundary.
        if old.is_dir() {
            return Err(VfsError::OperationNotPermitted);
        }
        self.check_search_trace(old_search, old_boundary, credentials)?;
        self.check_search_trace(new_search, new_boundary, credentials)?;
        // An existing target takes precedence over an unwritable parent.
        match new_dir.lookup_no_follow(new_name) {
            Ok(_) => return Err(VfsError::AlreadyExists),
            Err(VfsError::NotFound) => {}
            Err(error) => return Err(error),
        }
        self.check_mutation_parent_with_boundary_and_search(
            new_dir,
            new_search,
            new_boundary,
            credentials,
        )?;
        let old_metadata = old.metadata()?;
        if !credentials.cap_fowner && credentials.fsuid != old_metadata.uid {
            return Err(VfsError::OperationNotPermitted);
        }
        new_dir.link(new_name, old)
    }

    /// Creates a new symbolic link on the filesystem.
    pub fn symlink(
        &self,
        target: impl AsRef<str>,
        link_path: impl AsRef<Path>,
        uid: u32,
        gid: u32,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<Location> {
        let (dir, name, searched) =
            self.resolve_parent_with_search_checked(link_path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_root.as_ref(), credentials)
            })?;
        match dir.lookup_no_follow(&name) {
            Ok(_) => return Err(VfsError::AlreadyExists),
            Err(VfsError::NotFound) => {}
            Err(error) => return Err(error),
        }
        self.check_mutation_parent_with_search(&dir, &searched, credentials)?;
        dir.create_symlink(&name, target.as_ref(), NodePermission::default(), uid, gid)
    }

    /// Create a non-directory node after applying parent-directory DAC.
    pub fn create_node(
        &self,
        path: impl AsRef<Path>,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<Location> {
        let (dir, name, searched) =
            self.resolve_parent_with_search_checked(path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_root.as_ref(), credentials)
            })?;
        match dir.lookup_no_follow(&name) {
            Ok(_) => return Err(VfsError::AlreadyExists),
            Err(VfsError::NotFound) => {}
            Err(error) => return Err(error),
        }
        self.check_mutation_parent_with_search(&dir, &searched, credentials)?;
        dir.create(&name, node_type, permission, uid, gid)
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
