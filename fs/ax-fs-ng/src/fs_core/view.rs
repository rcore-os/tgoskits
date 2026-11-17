//! Operation-local path selection and its one immutable calling identity.

use alloc::{string::String, sync::Arc, vec::Vec};

use ax_io::{Read, Write};
use axfs_ng_vfs::{
    CreateAttributes, LinkCaller, Location, Metadata, MountNamespace, MutationCredentials,
    NativeLinkCaller, NodePermission, NodeType, PathHandle, RenameOptions, VfsError, VfsResult,
    path::{Path, PathBuf},
};

use super::{FinalComponent, FsContext, FsContextSnapshot, LookupPolicy, ParentLookup, ReadDir};
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
    pub fn permission_boundary(&self) -> Option<&PathHandle> {
        self.context().permission_boundary()
    }

    pub fn with_current_dir(&self, directory: PathHandle) -> VfsResult<Self> {
        let snapshot = self.snapshot.clone().with_current_directory(directory)?;
        Ok(Self::current(snapshot, self.caller).with_lookup_policy(self.policy))
    }

    pub fn with_dirfd(&self, directory: PathHandle) -> VfsResult<Self> {
        Self::at_directory(self.snapshot.clone(), self.caller, directory)
            .map(|view| view.with_lookup_policy(self.policy))
    }

    pub fn resolve(&self, path: impl AsRef<Path>) -> VfsResult<PathHandle> {
        self.resolve_with_search(path).map(|(location, _)| location)
    }
    pub fn resolve_no_follow(&self, path: impl AsRef<Path>) -> VfsResult<PathHandle> {
        self.resolve_no_follow_with_search(path)
            .map(|(location, _)| location)
    }
    pub fn resolve_with_search(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<(PathHandle, Vec<PathHandle>)> {
        self.resolve_with_search_checked(path, |_| Ok(()))
    }
    pub fn resolve_no_follow_with_search(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<(PathHandle, Vec<PathHandle>)> {
        self.resolve_no_follow_with_search_checked(path, |_| Ok(()))
    }
    pub fn resolve_checked(
        &self,
        path: impl AsRef<Path>,
        check: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<PathHandle> {
        self.resolve_with_search_checked(path, check)
            .map(|(location, _)| location)
    }
    pub fn resolve_no_follow_checked(
        &self,
        path: impl AsRef<Path>,
        check: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<PathHandle> {
        self.resolve_no_follow_with_search_checked(path, check)
            .map(|(location, _)| location)
    }
    pub fn resolve_with_search_checked(
        &self,
        path: impl AsRef<Path>,
        check: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<(PathHandle, Vec<PathHandle>)> {
        super::LookupSession::new(self, path.as_ref().as_str())?
            .with_search(&check)
            .parent()?
            .finish_lookup_with_trace(true)
    }
    pub fn resolve_no_follow_with_search_checked(
        &self,
        path: impl AsRef<Path>,
        check: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<(PathHandle, Vec<PathHandle>)> {
        super::LookupSession::new(self, path.as_ref().as_str())?
            .with_search(&check)
            .parent()?
            .finish_lookup_with_trace(false)
    }
    /// Resolves the parent without consuming or normalizing the last component.
    pub fn resolve_parent(&self, path: impl AsRef<Path>) -> VfsResult<ParentLookup> {
        self.resolve_parent_checked(path, |_| Ok(()))
    }

    pub fn resolve_parent_checked(
        &self,
        path: impl AsRef<Path>,
        check: impl Fn(&Location) -> VfsResult<()>,
    ) -> VfsResult<ParentLookup> {
        super::LookupSession::new(self, path.as_ref().as_str())?
            .with_search(&check)
            .parent()?
            .finish_parent(self.permission_boundary().cloned())
    }

    /// Retrieves metadata for the file.
    pub fn metadata(&self, path: impl AsRef<Path>) -> VfsResult<Metadata> {
        self.resolve(path)?.location().metadata()
    }

    /// Reads a link using this operation's caller while retaining its path use.
    /// Intermediate links are followed, but the final link itself is displayed.
    pub fn read_link(&self, path: impl AsRef<Path>) -> VfsResult<String> {
        let (path, _searched) = super::LookupSession::new(self, path.as_ref().as_str())?
            .parent()?
            .finish_lookup_with_trace(false)?;
        if path.location().node_type() != NodeType::Symlink {
            return Err(VfsError::InvalidInput);
        }
        path.location().read_link(self.caller)
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
        ReadDir::from_path(self.resolve(path)?)
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
        boundary: Option<&PathHandle>,
    ) -> VfsResult<()> {
        let credentials = self.credentials();
        let is_boundary = |location: &Location| {
            boundary.is_some_and(|boundary| location.ptr_eq(boundary.location()))
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
        searched: &[PathHandle],
        boundary: Option<&PathHandle>,
    ) -> VfsResult<()> {
        for directory in searched {
            self.check_search_path(directory.location(), boundary)?;
        }
        Ok(())
    }

    /// Removes a file from the original parent naming slot.
    pub fn remove_file(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        let parent = self.resolve_parent_checked(path, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        self.check_search_trace(&parent.searched, parent.boundary.as_ref())?;
        let FinalComponent::Name(name) = &parent.component else {
            return Err(VfsError::IsADirectory);
        };
        let entry = parent.directory.location().lookup_no_follow(name)?;
        if entry.is_dir() {
            return Err(VfsError::IsADirectory);
        }
        if parent.directory_required {
            return Err(VfsError::NotADirectory);
        }
        if entry.is_root_of_mount() {
            return Err(VfsError::ResourceBusy);
        }
        // Native backends classify active inode holders. The preflight entry
        // must retire before the actual unlink attempt observes that state.
        drop(entry);
        parent
            .directory
            .location()
            .unlink(name, false, self.credentials())
    }

    /// Removes an empty directory without following its last component.
    pub fn remove_dir(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        let parent = self.resolve_parent_checked(path, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        self.check_search_trace(&parent.searched, parent.boundary.as_ref())?;
        let name = match &parent.component {
            FinalComponent::Name(name) => name,
            FinalComponent::Current => return Err(VfsError::InvalidInput),
            FinalComponent::Parent => return Err(VfsError::DirectoryNotEmpty),
            FinalComponent::Root => return Err(VfsError::ResourceBusy),
        };
        let entry = parent.directory.location().lookup_no_follow(name)?;
        entry.check_is_dir()?;
        if entry.ptr_eq(self.root_dir()) || entry.is_root_of_mount() {
            return Err(VfsError::ResourceBusy);
        }
        drop(entry);
        parent
            .directory
            .location()
            .unlink(name, true, self.credentials())
    }

    /// Renames a file or directory, replacing an existing destination.
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
        let source = self.resolve_parent_checked(from, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        let destination = self.resolve_parent_checked(to, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        self.rename_resolved(&source, &destination, options)
    }

    /// Commits a rename using the original typed parent selections.
    pub fn rename_resolved(
        &self,
        source: &ParentLookup,
        destination: &ParentLookup,
        options: RenameOptions,
    ) -> VfsResult<()> {
        self.check_search_trace(&source.searched, source.boundary.as_ref())?;
        self.check_search_trace(&destination.searched, destination.boundary.as_ref())?;
        if !Arc::ptr_eq(
            source.directory.location().mountpoint(),
            destination.directory.location().mountpoint(),
        ) {
            return Err(VfsError::CrossesDevices);
        }
        let FinalComponent::Name(source_name) = &source.component else {
            return Err(VfsError::ResourceBusy);
        };
        let FinalComponent::Name(destination_name) = &destination.component else {
            return Err(if options.no_replace() {
                VfsError::AlreadyExists
            } else {
                VfsError::ResourceBusy
            });
        };
        self.check_search_path(source.directory.location(), source.boundary.as_ref())?;
        self.check_search_path(
            destination.directory.location(),
            destination.boundary.as_ref(),
        )?;
        let source_type = source
            .directory
            .location()
            .lookup_no_follow(source_name)?
            .node_type();
        let destination_type = match destination
            .directory
            .location()
            .lookup_no_follow(destination_name)
        {
            Ok(entry) => Some(entry.node_type()),
            Err(VfsError::NotFound) => None,
            Err(error) => return Err(error),
        };
        if options.no_replace() && destination_type.is_some() {
            return Err(VfsError::AlreadyExists);
        }
        if (source.directory_required || destination.directory_required)
            && source_type != NodeType::Directory
            || destination.directory_required
                && destination_type.is_some_and(|ty| ty != NodeType::Directory)
        {
            return Err(VfsError::NotADirectory);
        }
        // These observations only establish preflight ordering. The backend
        // still checks the current slot, write permission and sticky ownership.
        source.directory.location().rename_with_options(
            source_name,
            destination.directory.location(),
            destination_name,
            options,
            self.credentials(),
        )
    }

    fn creation_name<'parent>(
        &self,
        parent: &'parent ParentLookup,
        node_type: NodeType,
    ) -> VfsResult<&'parent str> {
        self.check_search_trace(&parent.searched, parent.boundary.as_ref())?;
        let FinalComponent::Name(name) = &parent.component else {
            return Err(VfsError::AlreadyExists);
        };
        if parent.directory_required && node_type != NodeType::Directory {
            return match parent.directory.location().lookup_no_follow(name) {
                Ok(_) => Err(VfsError::AlreadyExists),
                Err(error) => Err(error),
            };
        }
        Ok(name)
    }

    /// Creates a new, empty directory at the provided path.
    pub fn create_dir(
        &self,
        path: impl AsRef<Path>,
        mode: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<PathHandle> {
        let parent = self.resolve_parent_checked(path, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        let name = self.creation_name(&parent, NodeType::Directory)?;
        parent.directory.rebind(parent.directory.location().create(
            name,
            NodeType::Directory,
            mode,
            uid,
            gid,
            self.credentials(),
        )?)
    }

    /// Creates a new hard link on the filesystem.
    pub fn link(
        &self,
        old_path: impl AsRef<Path>,
        new_path: impl AsRef<Path>,
    ) -> VfsResult<PathHandle> {
        let (old, old_searched) = self.resolve_with_search_checked(old_path, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        let destination = self.resolve_parent_checked(new_path, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        self.link_resolved(
            (&old, &old_searched, self.permission_boundary()),
            &destination,
        )
    }

    /// Links an owned source into its original typed destination selection.
    pub fn link_resolved(
        &self,
        source: (&PathHandle, &[PathHandle], Option<&PathHandle>),
        destination: &ParentLookup,
    ) -> VfsResult<PathHandle> {
        let (old, searched, boundary) = source;
        if old.location().is_dir() {
            return Err(VfsError::OperationNotPermitted);
        }
        self.check_search_trace(searched, boundary)?;
        let name = self.creation_name(destination, old.location().node_type())?;
        destination
            .directory
            .rebind(destination.directory.location().link(
                name,
                old.location(),
                self.credentials(),
            )?)
    }

    /// Creates a new symbolic link on the filesystem.
    pub fn symlink(
        &self,
        target: impl AsRef<str>,
        link_path: impl AsRef<Path>,
        uid: u32,
        gid: u32,
    ) -> VfsResult<PathHandle> {
        let parent = self.resolve_parent_checked(link_path, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        let name = self.creation_name(&parent, NodeType::Symlink)?;
        parent
            .directory
            .rebind(parent.directory.location().create_symlink(
                name,
                target.as_ref(),
                NodePermission::default(),
                uid,
                gid,
                self.credentials(),
            )?)
    }

    /// Creates a non-directory node after applying parent-directory DAC.
    pub fn create_node(
        &self,
        path: impl AsRef<Path>,
        attributes: CreateAttributes<'_>,
    ) -> VfsResult<PathHandle> {
        let parent = self.resolve_parent_checked(path, |directory| {
            self.check_search_path(directory, self.permission_boundary())
        })?;
        let node_type = match attributes.content {
            axfs_ng_vfs::CreateContent::Node(node_type, _) => node_type,
            axfs_ng_vfs::CreateContent::Symlink(_) => NodeType::Symlink,
        };
        let name = self.creation_name(&parent, node_type)?;
        parent
            .directory
            .rebind(parent.directory.location().create_initialized(
                name,
                attributes,
                self.credentials(),
            )?)
    }

    /// Returns the canonical, absolute form of a path.
    pub fn canonicalize(&self, path: impl AsRef<Path>) -> VfsResult<PathBuf> {
        self.resolve(path.as_ref())?.location().absolute_path()
    }
}
