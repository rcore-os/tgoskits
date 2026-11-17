//! Operation-local path selection and its one immutable calling identity.

use alloc::{borrow::Cow, string::String, sync::Arc, vec::Vec};

use ax_io::{Read, Write};
use axfs_ng_vfs::{
    CreateAttributes, LinkCaller, Location, Metadata, MountNamespace, MutationCredentials,
    NativeLinkCaller, NodePermission, NodeType, PathHandle, RenameOptions, VfsError, VfsResult,
    path::{Path, PathBuf},
};

use super::{FsContext, FsContextSnapshot, LookupPolicy, ReadDir};
use crate::file::File;

/// Frozen paths and the caller that authorizes this operation.
///
/// Construction never reads a current task, a descriptor table or user memory.
/// The caller and its credential projection come from the same saved identity.
/// This view retains paths, not a mutable filesystem-owner share.
#[derive(Clone)]
pub struct FsView<'caller> {
    snapshot: FsContextSnapshot,
    caller: &'caller dyn LinkCaller,
    credentials: MutationCredentials<'caller>,
    policy: LookupPolicy,
}

impl<'caller> FsView<'caller> {
    /// Selects the supplied snapshot's cwd without acquiring another mount use.
    pub fn current(snapshot: FsContextSnapshot, caller: &'caller dyn LinkCaller) -> Self {
        Self {
            snapshot,
            caller,
            credentials: caller.credentials(),
            policy: LookupPolicy::default(),
        }
    }

    /// Selects an already-owned directory as the relative starting point.
    ///
    /// Call outside task/scope and namespace writer guards. The directory check
    /// and failed-input retirement happen here; process root remains unchanged.
    pub fn at_directory(
        snapshot: FsContextSnapshot,
        caller: &'caller dyn LinkCaller,
        start: PathHandle,
    ) -> VfsResult<Self> {
        Ok(Self::current(snapshot.with_start_directory(start)?, caller))
    }

    /// Uses explicitly selected native paths and identity, without a user fallback.
    pub fn native(context: FsContext, caller: &'caller NativeLinkCaller) -> Self {
        Self::current(FsContextSnapshot::native(context), caller)
    }

    pub fn root(&self) -> &PathHandle {
        self.snapshot.root()
    }

    pub fn cwd(&self) -> &PathHandle {
        self.snapshot.cwd()
    }

    pub fn namespace(&self) -> &Arc<MountNamespace> {
        self.snapshot.namespace()
    }

    /// Borrows the immutable DAC projection selected with the link caller.
    pub fn credentials(&self) -> &MutationCredentials<'caller> {
        &self.credentials
    }

    pub(super) fn context(&self) -> &FsContext {
        &self.snapshot
    }

    pub(super) fn caller(&self) -> &'caller dyn LinkCaller {
        self.caller
    }

    /// Applies traversal constraints without changing the selected root/cwd.
    pub fn with_lookup_policy(&self, policy: LookupPolicy) -> Self {
        Self {
            policy,
            ..self.clone()
        }
    }

    pub(super) fn lookup_policy(&self) -> LookupPolicy {
        self.policy
    }
    pub fn root_dir(&self) -> &Location {
        self.root().location()
    }
    pub fn current_dir(&self) -> &Location {
        self.cwd().location()
    }
    pub fn mount_namespace(&self) -> &Arc<MountNamespace> {
        self.namespace()
    }
    pub fn permission_boundary(&self) -> Option<&Location> {
        self.context().permission_boundary()
    }

    pub fn with_current_dir(&self, directory: Location) -> VfsResult<Self> {
        let snapshot = self.snapshot.clone().with_current_directory(directory)?;
        Ok(Self::current(snapshot, self.caller).with_lookup_policy(self.policy))
    }

    pub fn with_dirfd(&self, directory: PathHandle) -> VfsResult<Self> {
        Self::at_directory(self.snapshot.clone(), self.caller, directory)
            .map(|view| view.with_lookup_policy(self.policy))
    }

    pub fn resolve(&self, path: impl AsRef<Path>) -> VfsResult<Location> {
        self.resolve_with_search(path).map(|(location, _)| location)
    }
    pub fn resolve_no_follow(&self, path: impl AsRef<Path>) -> VfsResult<Location> {
        self.resolve_no_follow_with_search(path)
            .map(|(location, _)| location)
    }
    pub fn resolve_with_search(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<(Location, Vec<Location>)> {
        self.resolve_with_search_checked(path, |_| Ok(()))
    }
    pub fn resolve_no_follow_with_search(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<(Location, Vec<Location>)> {
        self.resolve_no_follow_with_search_checked(path, |_| Ok(()))
    }
    pub fn resolve_checked(
        &self,
        path: impl AsRef<Path>,
        check: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<Location> {
        self.resolve_with_search_checked(path, check)
            .map(|(location, _)| location)
    }
    pub fn resolve_no_follow_checked(
        &self,
        path: impl AsRef<Path>,
        check: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<Location> {
        self.resolve_no_follow_with_search_checked(path, check)
            .map(|(location, _)| location)
    }
    pub fn resolve_with_search_checked(
        &self,
        path: impl AsRef<Path>,
        check: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<(Location, Vec<Location>)> {
        super::LookupSession::new(self, path.as_ref().as_str())?
            .with_search(&check)
            .parent()?
            .finish_lookup_with_trace(true)
    }
    pub fn resolve_no_follow_with_search_checked(
        &self,
        path: impl AsRef<Path>,
        check: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<(Location, Vec<Location>)> {
        super::LookupSession::new(self, path.as_ref().as_str())?
            .with_search(&check)
            .parent()?
            .finish_lookup_with_trace(false)
    }
    pub fn resolve_parent_with_search<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(Location, Cow<'a, str>, Vec<Location>)> {
        self.resolve_parent_with_search_checked(path, |_| Ok(()))
    }
    pub fn resolve_parent_with_search_checked<'a>(
        &self,
        path: &'a Path,
        check: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<(Location, Cow<'a, str>, Vec<Location>)> {
        let (directory, name, searched) = super::LookupSession::new(self, path.as_str())?
            .with_search(&check)
            .parent()?
            .finish_parent()?;
        Ok((directory, Cow::Owned(name), searched))
    }
    pub fn resolve_parent<'a>(&self, path: &'a Path) -> VfsResult<(Location, Cow<'a, str>)> {
        self.resolve_parent_with_search(path)
            .map(|(directory, name, _)| (directory, name))
    }
    // Convert an already resolved final entry into a naming pair. This is not a
    // walker step: scoped traversal must inspect each ParentStep itself.
    pub(super) fn resolved_parent_name(
        location: &Location,
    ) -> VfsResult<Option<(Location, String)>> {
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
    ) -> VfsResult<()> {
        let credentials = self.credentials();
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
    ) -> VfsResult<()> {
        for directory in searched {
            self.check_search_path(directory, boundary)?;
        }
        Ok(())
    }

    /// Removes a file from the filesystem.
    pub fn remove_file(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        let credentials = self.credentials();
        let (entry, searched) = self
            .resolve_no_follow_with_search_checked(path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_boundary())
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
        self.check_search_trace(&searched, self.permission_boundary())?;
        drop(entry);
        drop(searched);
        directory.unlink(&name, false, credentials)
    }

    /// Removes a directory from the filesystem.
    pub fn remove_dir(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        let credentials = self.credentials();
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
                self.check_search_path(directory, self.permission_boundary())
            })?;
            self.check_search_trace(&searched, self.permission_boundary())?;
            return Err(if last == "." {
                VfsError::InvalidInput
            } else {
                VfsError::DirectoryNotEmpty
            });
        }

        let (entry, searched) = self.resolve_no_follow_with_search_checked(path, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        if entry.ptr_eq(self.root_dir()) || entry.is_root_of_mount() {
            return Err(VfsError::ResourceBusy);
        }
        let (directory, name) =
            Self::resolved_parent_name(&entry)?.ok_or(VfsError::ResourceBusy)?;
        self.check_search_trace(&searched, self.permission_boundary())?;
        drop(entry);
        drop(searched);
        directory.unlink(&name, true, credentials)
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
        let (src_dir, src_name, src_searched) = self
            .resolve_parent_with_search_checked(from.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_boundary())
            })?;
        let (dst_dir, dst_name, dst_searched) = self
            .resolve_parent_with_search_checked(to.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_boundary())
            })?;
        self.rename_locations_with_boundaries_and_search(
            (&src_dir, &src_name),
            (&dst_dir, &dst_name),
            options,
            (self.permission_boundary(), self.permission_boundary()),
            (&src_searched, &dst_searched),
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
    ) -> VfsResult<()> {
        self.rename_locations_with_boundaries(
            (src_dir, src_name),
            (dst_dir, dst_name),
            options,
            (self.permission_boundary(), self.permission_boundary()),
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
    ) -> VfsResult<()> {
        let (src_dir, src_name) = source;
        let (dst_dir, dst_name) = destination;
        self.rename_locations_with_boundaries_and_search(
            (src_dir, src_name),
            (dst_dir, dst_name),
            options,
            boundaries,
            (&[], &[]),
        )
    }

    pub fn rename_locations_with_boundaries_and_search(
        &self,
        source: (&Location, &str),
        destination: (&Location, &str),
        options: RenameOptions,
        boundaries: (Option<&Location>, Option<&Location>),
        searches: (&[Location], &[Location]),
    ) -> VfsResult<()> {
        let credentials = self.credentials();
        let (src_dir, src_name) = source;
        let (dst_dir, dst_name) = destination;
        let (src_boundary, dst_boundary) = boundaries;
        let (src_search, dst_search) = searches;
        // Search permission is a prerequisite for looking up either final
        // entry. Keep this inside the VFS operation so callers cannot observe
        // ENOENT/EEXIST from an inaccessible parent before DAC search checks.
        self.check_search_trace(src_search, src_boundary)?;
        self.check_search_trace(dst_search, dst_boundary)?;
        if !Arc::ptr_eq(src_dir.mountpoint(), dst_dir.mountpoint()) {
            return Err(VfsError::CrossesDevices);
        }
        // Already-resolved callers may have no search trace. Validate both
        // parent paths before probing names, without deciding write access.
        self.check_search_path(src_dir, src_boundary)?;
        self.check_search_path(dst_dir, dst_boundary)?;
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
    ) -> VfsResult<Location> {
        let credentials = self.credentials();
        let path = path.as_ref();
        if path.as_str().is_empty() {
            return Err(VfsError::NotFound);
        }
        let (dir, name, searched) = self.resolve_parent_with_search_checked(path, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        self.check_search_trace(&searched, self.permission_boundary())?;
        dir.create(&name, NodeType::Directory, mode, uid, gid, credentials)
    }

    /// Creates a new hard link on the filesystem.
    pub fn link(
        &self,
        old_path: impl AsRef<Path>,
        new_path: impl AsRef<Path>,
    ) -> VfsResult<Location> {
        let (old, old_searched) = self
            .resolve_with_search_checked(old_path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_boundary())
            })?;
        let (new_dir, new_name, new_searched) = self
            .resolve_parent_with_search_checked(new_path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_boundary())
            })?;
        self.link_locations_with_boundaries_and_search(
            (&old, &old_searched),
            (&new_dir, &new_name, &new_searched),
            (self.permission_boundary(), self.permission_boundary()),
        )
    }

    /// Create a hard link between already resolved locations.
    pub fn link_locations(
        &self,
        old: &Location,
        new_dir: &Location,
        new_name: &str,
    ) -> VfsResult<Location> {
        self.link_locations_with_boundaries_and_search(
            (old, &[]),
            (new_dir, new_name, &[]),
            (self.permission_boundary(), self.permission_boundary()),
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
    ) -> VfsResult<Location> {
        self.link_locations_with_boundaries_and_search(
            (old, &[]),
            (new_dir, new_name, &[]),
            (old_boundary, new_boundary),
        )
    }

    pub fn link_locations_with_boundaries_and_search(
        &self,
        source: (&Location, &[Location]),
        destination: (&Location, &str, &[Location]),
        boundaries: (Option<&Location>, Option<&Location>),
    ) -> VfsResult<Location> {
        let credentials = self.credentials();
        let (old, old_search) = source;
        let (new_dir, new_name, new_search) = destination;
        let (old_boundary, new_boundary) = boundaries;
        // A directory cannot be hard-linked. Check this before inspecting
        // the source parent: `linkat(dirfd, ".", ...)` must not re-walk a
        // parent outside the dirfd permission boundary.
        if old.is_dir() {
            return Err(VfsError::OperationNotPermitted);
        }
        self.check_search_trace(old_search, old_boundary)?;
        self.check_search_trace(new_search, new_boundary)?;
        new_dir.link(new_name, old, credentials)
    }

    /// Creates a new symbolic link on the filesystem.
    pub fn symlink(
        &self,
        target: impl AsRef<str>,
        link_path: impl AsRef<Path>,
        uid: u32,
        gid: u32,
    ) -> VfsResult<Location> {
        let credentials = self.credentials();
        let (dir, name, searched) = self
            .resolve_parent_with_search_checked(link_path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_boundary())
            })?;
        self.check_search_trace(&searched, self.permission_boundary())?;
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
    ) -> VfsResult<Location> {
        let credentials = self.credentials();
        let (dir, name, searched) = self
            .resolve_parent_with_search_checked(path.as_ref(), |directory| {
                self.check_search_path(directory, self.permission_boundary())
            })?;
        self.check_search_trace(&searched, self.permission_boundary())?;
        dir.create_initialized(&name, attributes, credentials)
    }

    /// Returns the canonical, absolute form of a path.
    pub fn canonicalize(&self, path: impl AsRef<Path>) -> VfsResult<PathBuf> {
        self.resolve(path.as_ref())?.absolute_path()
    }
}
