use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    collections::vec_deque::VecDeque,
    string::String,
    sync::Arc,
    vec::Vec,
};

use ax_io::{Read, Write};
use ax_lazyinit::OnceLock;
use axfs_ng_vfs::{
    CreateAttributes, DirectoryCursor, DirectoryReadState, Location, Metadata, MountNamespace,
    Mountpoint, MutationCredentials, NodePermission, NodeType, PathHandle, RenameOptions, VfsError,
    VfsResult,
    path::{Component, Components, Path, PathBuf},
};

use super::{FsContextOwner, FsContextSnapshot};
use crate::file::File;

type SearchCheck<'a> = Option<&'a dyn Fn(&Location) -> VfsResult<()>>;

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
    permission_root: Option<Location>,
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

    // Convert an already resolved final entry into a naming pair. This is not a
    // walker step: scoped traversal must inspect each ParentStep itself.
    fn resolved_parent_name(location: &Location) -> VfsResult<Option<(Location, String)>> {
        let mut current = location.clone();
        loop {
            match current.parent_step()? {
                axfs_ng_vfs::ParentStep::Parent { parent, name } => {
                    return Ok(Some((parent, name)));
                }
                axfs_ng_vfs::ParentStep::Covered(covered) => current = covered,
                axfs_ng_vfs::ParentStep::Root => return Ok(None),
                axfs_ng_vfs::ParentStep::Unlinked { parent, name } => {
                    return Ok(parent.map(|parent| (parent, name)));
                }
            }
        }
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

    /// Selects a different relative starting directory without changing task state.
    pub fn with_current_dir(&self, current_dir: Location) -> VfsResult<Self> {
        current_dir.check_is_dir()?;
        let current_dir = if alloc::sync::Arc::ptr_eq(
            self.current_dir().mountpoint(),
            current_dir.mountpoint(),
        ) {
            self.current_dir.rebind(current_dir)?
        } else {
            PathHandle::acquire(current_dir)?
        };
        let mut view = Self::from_paths(self.namespace.clone(), self.root_dir.clone(), current_dir);
        view.permission_root = self.permission_root.clone();
        Ok(view)
    }

    /// Selects a directory fd as a permission boundary, preserving process root.
    pub fn with_dirfd(&self, dirfd: Location) -> VfsResult<Self> {
        let mut view = self.with_current_dir(dirfd.clone())?;
        view.permission_root = Some(dirfd);
        Ok(view)
    }

    /// Returns the directory boundary used for relative-path permissions.
    pub fn permission_boundary(&self) -> Option<&Location> {
        self.permission_root.as_ref()
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

    /// Recognizes the saved process root even below covering mounts. A dirfd
    /// only changes permission lookup and must not become a containment root.
    fn is_process_root(&self, directory: &Location) -> bool {
        if directory.ptr_eq(self.root_dir()) {
            return true;
        }
        if !directory.is_root_of_mount() {
            return false;
        }
        // parent() skips covered mount roots. Check each identity before the
        // walk can leave the process root, including stacked covering mounts.
        let mut covered = directory.mountpoint().location();
        while let Some(location) = covered {
            if location.ptr_eq(self.root_dir()) {
                return true;
            }
            if !location.is_root_of_mount() {
                break;
            }
            covered = location.mountpoint().location();
        }
        false
    }

    fn resolve_components(
        &self,
        components: Components,
        follow_count: &mut usize,
        search: SearchCheck<'_>,
    ) -> VfsResult<Location> {
        let mut dir = self.current_dir().clone();
        for comp in components {
            match comp {
                Component::CurDir => {}
                Component::ParentDir => {
                    Self::check_search(&dir, search)?;
                    if !self.is_process_root(&dir) {
                        dir = dir.parent().unwrap_or_else(|| self.root_dir().clone());
                    }
                }
                Component::RootDir => {
                    dir = self.root_dir().clone();
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
        let mut dir = self.current_dir().clone();
        for comp in components {
            match comp {
                Component::CurDir => {}
                Component::ParentDir => {
                    searched.push(dir.clone());
                    Self::check_search(&dir, search)?;
                    if !self.is_process_root(&dir) {
                        dir = dir.parent().unwrap_or_else(|| self.root_dir().clone());
                    }
                }
                Component::RootDir => {
                    dir = self.root_dir().clone();
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
        // A final `..` returns its parent without searching that parent.
        // Only record it when a final name or explicit dot still needs search.
        if entry_name.is_some() || Self::ends_in_dot(path) {
            searched.push(dir.clone());
            Self::check_search(&dir, search)?;
        }
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
        } else if self.is_process_root(&dir) {
            Err(VfsError::InvalidInput)
        } else if let Some((parent, name)) = Self::resolved_parent_name(&dir)? {
            // `parent` is only used to represent the resolved final `.`/`..`
            // entry as `(parent, name)`. It was not traversed by the path
            // walk, so it must not be added to the search trace. This is
            // essential for an already-open dirfd whose own parent is not
            // searchable.
            Ok((parent, name.into(), searched))
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
        let mut dir = self.current_dir().clone();
        let mut depth = 0usize;

        while let Some(component) = components.next() {
            let is_last = components.peek().is_none();
            match component {
                Component::RootDir => return Err(VfsError::CrossesDevices),
                Component::CurDir if is_last => {
                    check_search(&dir)?;
                    if let Some((parent, name)) = Self::resolved_parent_name(&dir)? {
                        return Ok((parent, name.into()));
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
                        if let Some((parent, name)) = Self::resolved_parent_name(&dir)? {
                            return Ok((parent, name.into()));
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
            if self.is_process_root(&dir) {
                Err(VfsError::InvalidInput)
            } else if let Some((parent, name)) = Self::resolved_parent_name(&dir)? {
                Ok((parent, name.into()))
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
    pub fn write(
        &self,
        path: impl AsRef<Path>,
        buf: impl AsRef<[u8]>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        let file = File::create(self, path.as_ref(), credentials)?;
        (&file)
            .write_all(buf.as_ref())
            .map_err(crate::io_error_to_vfs_error)?;
        Ok(())
    }

    /// Returns an iterator over the entries in a directory.
    pub fn read_dir(&self, path: impl AsRef<Path>) -> VfsResult<ReadDir> {
        ReadDir::from_location(self.resolve(path)?)
    }

    /// Check one DAC permission class for a filesystem location.
    pub(crate) fn check_permission(
        location: &Location,
        credentials: &MutationCredentials<'_>,
        required: NodePermission,
    ) -> VfsResult<()> {
        if credentials.cap_dac_override {
            return Ok(());
        }
        if credentials.cap_dac_read_search && required.bits() & !0o555 == 0 {
            return Ok(());
        }
        credentials.check_permission(&location.metadata()?, required)
    }

    /// Check execute/search permission up to the selected path boundary.
    pub fn check_search_path(
        &self,
        directory: &Location,
        boundary: Option<&Location>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<()> {
        let is_boundary = |location: &Location| {
            boundary.is_some_and(|boundary| location.ptr_eq(boundary))
                || location.ptr_eq(self.root_dir())
        };
        let mut current = directory.clone();
        loop {
            Self::check_permission(&current, credentials, NodePermission::OTHER_EXEC)?;
            if is_boundary(&current) {
                return Ok(());
            }
            if current.is_root_of_mount() {
                // A mount can cover the saved process root or dirfd boundary.
                // Check those identities before parent() skips past them, but
                // do not require search permission on hidden covered inodes.
                let mut covered = current.mountpoint().location();
                while let Some(location) = covered {
                    if is_boundary(&location) {
                        return Ok(());
                    }
                    if !location.is_root_of_mount() {
                        break;
                    }
                    covered = location.mountpoint().location();
                }
            }
            current = current.parent().ok_or(VfsError::InvalidInput)?;
        }
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
        if entry.ptr_eq(self.root_dir()) {
            return Err(VfsError::IsADirectory);
        }
        // The final entry is already known to be a directory. Classify this
        // before consulting its physical parent: an opened dirfd may remain
        // usable after that parent loses search permission.
        if entry.is_dir() {
            return Err(VfsError::IsADirectory);
        }
        let (directory, name) =
            Self::resolved_parent_name(&entry)?.ok_or(VfsError::IsADirectory)?;
        self.check_search_trace(&searched, self.permission_root.as_ref(), credentials)?;
        drop(entry);
        drop(searched);
        directory.unlink(&name, false, credentials)
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
            // For an exact dirfd-relative `.`/`..`, the final object is
            // determined at the open directory boundary. Do not walk its
            // physical parent, which may have become unsearchable after the
            // dirfd was opened.
            if matches!(trimmed, "." | "..") {
                return Err(if last == "." {
                    VfsError::InvalidInput
                } else {
                    VfsError::DirectoryNotEmpty
                });
            }
            let (_, searched) = self.resolve_no_follow_with_search_checked(path, |directory| {
                self.check_search_path(directory, self.permission_root.as_ref(), credentials)
            })?;
            self.check_search_trace(&searched, self.permission_root.as_ref(), credentials)?;
            return Err(if last == "." {
                VfsError::InvalidInput
            } else {
                VfsError::DirectoryNotEmpty
            });
        }

        let (entry, searched) = self.resolve_no_follow_with_search_checked(path, |directory| {
            self.check_search_path(directory, self.permission_root.as_ref(), credentials)
        })?;
        if entry.ptr_eq(self.root_dir()) || entry.is_root_of_mount() {
            return Err(VfsError::ResourceBusy);
        }
        let (directory, name) =
            Self::resolved_parent_name(&entry)?.ok_or(VfsError::ResourceBusy)?;
        self.check_search_trace(&searched, self.permission_root.as_ref(), credentials)?;
        drop(entry);
        drop(searched);
        directory.unlink(&name, true, credentials)
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
        if !Arc::ptr_eq(src_dir.mountpoint(), dst_dir.mountpoint()) {
            return Err(VfsError::CrossesDevices);
        }
        // Already-resolved callers may have no search trace. Validate both
        // parent paths before probing names, without deciding write access.
        self.check_search_path(src_dir, src_boundary, credentials)?;
        self.check_search_path(dst_dir, dst_boundary, credentials)?;
        // Preserve preflight error ordering; none of these observations grants
        // permission to commit. The backend attempt rechecks current objects.
        src_dir.lookup_no_follow(src_name)?;
        let destination = match dst_dir.lookup_no_follow(dst_name) {
            Ok(destination) => Some(destination),
            Err(VfsError::NotFound) => None,
            Err(error) => return Err(error),
        };

        // Once both final entries have been searched, an existing destination
        // has priority over destination write permission for NOREPLACE.
        if options.no_replace() && destination.is_some() {
            return Err(VfsError::AlreadyExists);
        }

        // Parent write, sticky, and no-op decisions all depend on the locked
        // current objects. An outer write check could reject a legal no-op
        // after a competing move, even if these probes saw different inodes.
        src_dir.rename_with_options(src_name, dst_dir, dst_name, options, credentials)
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
        self.check_search_trace(&searched, self.permission_root.as_ref(), credentials)?;
        dir.create(&name, NodeType::Directory, mode, uid, gid, credentials)
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
        new_dir.link(new_name, old, credentials)
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
        self.check_search_trace(&searched, self.permission_root.as_ref(), credentials)?;
        dir.create_symlink(
            &name,
            target.as_ref(),
            NodePermission::default(),
            uid,
            gid,
            credentials,
        )
    }

    /// Create a non-directory node after applying parent-directory DAC.
    pub fn create_node(
        &self,
        path: impl AsRef<Path>,
        attributes: CreateAttributes<'_>,
        credentials: &MutationCredentials<'_>,
    ) -> VfsResult<Location> {
        let (dir, name, searched) =
            self.resolve_parent_with_search_checked(path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_root.as_ref(), credentials)
            })?;
        self.check_search_trace(&searched, self.permission_root.as_ref(), credentials)?;
        dir.create_initialized(&name, attributes, credentials)
    }

    /// Returns the canonical, absolute form of a path.
    pub fn canonicalize(&self, path: impl AsRef<Path>) -> VfsResult<PathBuf> {
        self.resolve(path.as_ref())?.absolute_path()
    }
}

/// Iterator returned by [`FsContext::read_dir`].
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

    pub(crate) fn from_location(dir: Location) -> VfsResult<Self> {
        let dir = PathHandle::acquire(dir)?;
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
