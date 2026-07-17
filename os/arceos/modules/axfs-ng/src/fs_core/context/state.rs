//! Filesystem-context state and path-resolution operations.

#[cfg(feature = "vfs")]
use alloc::sync::Arc;
use alloc::{borrow::Cow, string::String, vec::Vec};

use ax_io::{Read, Write};
#[cfg(feature = "vfs")]
use axfs_ng_vfs::Mountpoint;
use axfs_ng_vfs::{
    Location, Metadata, NodePermission, NodeType, VfsError, VfsResult,
    path::{Component, Components, Path, PathBuf},
};

#[cfg(feature = "vfs")]
use super::publication::MountNamespace;
use super::{
    operation::{FsContextOperationView, FsNamespaceOperationView},
    publication::registered_contexts,
    read_dir::ReadDir,
};
use crate::{
    file::{CachedFile, File, FileLocation, GenerationBoundLocation, UnmanagedLocation},
    lifecycle::{FsGeneration, FsOpenHandleLease, FsOperationLease, FsRuntime, FsRuntimeError},
};

/// Maximum number of symlinks that will be followed during path resolution.
pub const SYMLINKS_MAX: usize = 40;

/// Provides `std::fs`-like interface.
#[derive(Debug, Clone)]
pub struct FsContext {
    #[cfg(feature = "vfs")]
    mnt_ns: Arc<MountNamespace>,
    root_dir: Location,
    current_dir: Location,
    lifecycle: Option<FsContextLifecycle>,
}

#[derive(Debug, Clone)]
struct FsContextLifecycle {
    runtime: FsRuntime,
    generation: FsGeneration,
}

/// Opaque root transition produced by [`FsContext::pivot_root_paths`].
///
/// Both locations retain their original generation authority. The transition
/// exposes no VFS location and can only be consumed by
/// [`FsContext::propagate_pivot_root`].
pub struct PivotRootTransition {
    old_root: FileLocation,
    new_root: FileLocation,
}

impl FsContext {
    /// Creates a new context with `root_dir` as both root and current directory.
    pub fn new(root_dir: UnmanagedLocation) -> Self {
        Self::new_unmanaged(root_dir.into_inner())
    }

    fn new_unmanaged(root_dir: Location) -> Self {
        #[cfg(feature = "vfs")]
        {
            let mnt_ns = Arc::new(MountNamespace::new(root_dir.mountpoint().clone(), None));
            Self::new_in_namespace(mnt_ns, root_dir, None)
        }
        #[cfg(not(feature = "vfs"))]
        {
            Self {
                root_dir: root_dir.clone(),
                current_dir: root_dir,
                lifecycle: None,
            }
        }
    }

    pub(crate) fn new_managed(
        root_dir: Location,
        runtime: FsRuntime,
        generation: FsGeneration,
    ) -> Self {
        let lifecycle = Some(FsContextLifecycle {
            runtime,
            generation,
        });
        #[cfg(feature = "vfs")]
        {
            let namespace = Arc::new(MountNamespace::new(
                root_dir.mountpoint().clone(),
                lifecycle
                    .as_ref()
                    .map(|lifecycle| lifecycle.runtime.bind_generation_access(generation)),
            ));
            Self::new_in_namespace(namespace, root_dir, lifecycle)
        }
        #[cfg(not(feature = "vfs"))]
        {
            Self {
                root_dir: root_dir.clone(),
                current_dir: root_dir,
                lifecycle,
            }
        }
    }

    #[cfg(feature = "vfs")]
    fn new_in_namespace(
        mnt_ns: Arc<MountNamespace>,
        root_dir: Location,
        lifecycle: Option<FsContextLifecycle>,
    ) -> Self {
        Self {
            root_dir: root_dir.clone(),
            current_dir: root_dir,
            mnt_ns,
            lifecycle,
        }
    }

    /// Returns the mount generation represented by this context.
    pub fn generation(&self) -> Option<FsGeneration> {
        self.lifecycle
            .as_ref()
            .map(|lifecycle| lifecycle.generation)
    }

    /// Verifies that this context belongs to the mounted generation.
    pub fn validate_generation(&self) -> Result<(), FsRuntimeError> {
        self.lifecycle
            .as_ref()
            .map(|lifecycle| {
                lifecycle
                    .runtime
                    .begin_operation(lifecycle.generation)
                    .map(drop)
            })
            .transpose()
            .map(|_| ())
    }

    pub(crate) fn begin_operation(&self) -> VfsResult<Option<FsOperationLease>> {
        self.lifecycle
            .as_ref()
            .map(|lifecycle| {
                lifecycle
                    .runtime
                    .begin_operation(lifecycle.generation)
                    .map_err(map_lifecycle_error)
            })
            .transpose()
    }

    /// Runs a path or mount-namespace operation under one exact generation
    /// lease.
    ///
    /// Every resolved [`crate::LocationOperationView`] borrows this lease, so
    /// it cannot escape the callback. A freeze that starts after admission
    /// waits for the callback to finish; a callback entered after freeze fails
    /// with [`VfsError::BadState`].
    pub fn with_namespace_operation<T>(
        &self,
        operation: impl for<'operation> FnOnce(FsNamespaceOperationView<'operation>) -> VfsResult<T>,
    ) -> VfsResult<T> {
        let operation_lease = self.begin_operation()?;
        operation(FsNamespaceOperationView::new(
            self,
            operation_lease.as_ref(),
        ))
    }

    /// Runs a composite filesystem-context operation under one admission.
    ///
    /// Methods on the scoped view reuse this exact lease, so a freeze that
    /// starts after entry cannot split one logical operation at a nested
    /// admission point. The higher-ranked callback prevents the view from
    /// escaping the lease. Ordinary [`FsContext`] methods continue to admit
    /// their own independent operation.
    ///
    /// ```compile_fail
    /// use ax_fs_ng::vfs::FsContext;
    ///
    /// fn leak_scope(context: &FsContext) {
    ///     let _escaped = context.with_operation_scope(|scope| Ok(scope));
    /// }
    /// ```
    pub fn with_operation_scope<T>(
        &self,
        operation: impl for<'operation> FnOnce(FsContextOperationView<'operation>) -> VfsResult<T>,
    ) -> VfsResult<T> {
        let operation_lease = self.begin_operation()?;
        operation(FsContextOperationView::new(self, operation_lease.as_ref())?)
    }

    pub(crate) fn open_handle(&self) -> VfsResult<Option<FsOpenHandleLease>> {
        self.lifecycle
            .as_ref()
            .map(|lifecycle| {
                lifecycle
                    .runtime
                    .open_handle(lifecycle.generation)
                    .map_err(map_lifecycle_error)
            })
            .transpose()
    }

    pub(crate) fn open_handle_during(
        &self,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<Option<FsOpenHandleLease>> {
        match (&self.lifecycle, operation) {
            (Some(lifecycle), Some(operation)) => lifecycle
                .runtime
                .open_handle_during(lifecycle.generation, operation)
                .map(Some)
                .map_err(map_lifecycle_error),
            (None, None) => Ok(None),
            _ => Err(VfsError::BadState),
        }
    }

    pub(super) fn validate_operation(&self, operation: Option<&FsOperationLease>) -> VfsResult<()> {
        match (&self.lifecycle, operation) {
            (Some(lifecycle), Some(operation)) => lifecycle
                .runtime
                .validate_operation(lifecycle.generation, operation)
                .map_err(map_lifecycle_error),
            (None, None) => Ok(()),
            _ => Err(VfsError::BadState),
        }
    }

    pub(crate) fn resolve_parent_during<'a>(
        &self,
        path: &'a Path,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<(Location, Cow<'a, str>)> {
        self.validate_operation(operation)?;
        self.resolve_parent_active(path)
    }

    pub(crate) fn with_current_dir_during(
        &self,
        current_dir: Location,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<Self> {
        self.validate_operation(operation)?;
        self.clone_with_current_dir(current_dir)
    }

    pub(crate) fn try_resolve_symlink_during(
        &self,
        location: Location,
        follow_count: &mut usize,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<Location> {
        self.validate_operation(operation)?;
        self.try_resolve_symlink_active(location, follow_count)
    }

    /// Opens a cached backend for an already-resolved location.
    ///
    /// A managed location must already carry authority from this context's
    /// exact runtime and generation. The returned behavior handle owns a
    /// counted open-handle lease, so mappings and executable caches cloned
    /// from it keep filesystem handoff blocked until their final clone drops.
    /// Raw retained locations remain uncounted generation tokens.
    pub fn open_cached_location(&self, location: FileLocation) -> VfsResult<CachedFile> {
        match location {
            FileLocation::Managed(location) => {
                let lease = self.open_handle()?.ok_or(VfsError::BadState)?;
                let operation = lease.begin_operation().map_err(map_lifecycle_error)?;
                location
                    .validate_operation(&operation)
                    .map_err(map_lifecycle_error)?;
                self.validate_location_namespace(location.location())?;
                CachedFile::get_or_create_generation_bound(location, lease)
            }
            FileLocation::Unmanaged(location) => {
                if self.lifecycle.is_some() {
                    return Err(VfsError::BadState);
                }
                self.validate_location_namespace(location.as_inner())?;
                CachedFile::get_or_create(location)
            }
        }
    }

    /// Resolves a path and retains the capability needed to cache its location.
    pub fn resolve_file_location(&self, path: impl AsRef<Path>) -> VfsResult<FileLocation> {
        let operation = self.begin_operation()?;
        self.resolve_file_location_during(path.as_ref(), operation.as_ref())
    }

    pub(super) fn resolve_file_location_during(
        &self,
        path: &Path,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<FileLocation> {
        self.validate_operation(operation)?;
        let location = self.resolve_active(path)?;
        self.bind_file_location(location, operation)
    }

    /// Resolves a path without following its final symlink and retains its
    /// location capability.
    pub fn resolve_file_location_no_follow(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<FileLocation> {
        let operation = self.begin_operation()?;
        self.resolve_file_location_no_follow_during(path.as_ref(), operation.as_ref())
    }

    pub(super) fn resolve_file_location_no_follow_during(
        &self,
        path: &Path,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<FileLocation> {
        self.validate_operation(operation)?;
        let location = self.resolve_no_follow_active(path)?;
        self.bind_file_location(location, operation)
    }

    /// Resolves a path and proves that it belongs to a non-detachable
    /// synthetic filesystem.
    pub fn resolve_unmanaged_location(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<UnmanagedLocation> {
        let _operation = self.begin_operation()?;
        UnmanagedLocation::try_new(self.resolve_active(path)?).map_err(|_| VfsError::BadState)
    }

    /// Resolves a parent directory and retains generation authority for the
    /// operation that will consume it.
    pub fn resolve_parent_file_location<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(FileLocation, Cow<'a, str>)> {
        let operation = self.begin_operation()?;
        self.resolve_parent_file_location_during(path, operation.as_ref())
    }

    pub(super) fn resolve_parent_file_location_during<'a>(
        &self,
        path: &'a Path,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<(FileLocation, Cow<'a, str>)> {
        self.validate_operation(operation)?;
        let (directory, name) = self.resolve_parent_active(path)?;
        Ok((self.bind_file_location(directory, operation)?, name))
    }

    /// Resolves a parent for a not-yet-created entry and retains generation
    /// authority for the operation that will consume it.
    pub fn resolve_nonexistent_file_location<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(FileLocation, &'a str)> {
        let operation = self.begin_operation()?;
        self.resolve_nonexistent_file_location_during(path, operation.as_ref())
    }

    pub(super) fn resolve_nonexistent_file_location_during<'a>(
        &self,
        path: &'a Path,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<(FileLocation, &'a str)> {
        self.validate_operation(operation)?;
        let (directory, name) = self.resolve_nonexistent_active(path)?;
        Ok((self.bind_file_location(directory, operation)?, name))
    }

    pub(super) fn bind_file_location(
        &self,
        location: Location,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<FileLocation> {
        match &self.lifecycle {
            Some(lifecycle) => {
                let operation = operation.ok_or(VfsError::BadState)?;
                let access = lifecycle
                    .runtime
                    .continue_generation_access(lifecycle.generation, operation)
                    .map_err(map_lifecycle_error)?;
                Ok(FileLocation::Managed(GenerationBoundLocation::from_access(
                    location, access,
                )))
            }
            None => UnmanagedLocation::try_new(location)
                .map(FileLocation::Unmanaged)
                .map_err(|_| VfsError::InvalidInput),
        }
    }

    /// Returns the mount namespace backing this filesystem context.
    #[cfg(feature = "vfs")]
    pub fn mount_namespace(&self) -> &Arc<MountNamespace> {
        &self.mnt_ns
    }

    #[cfg(feature = "vfs")]
    pub(super) fn mount_namespace_contains(&self, mountpoint: &Arc<Mountpoint>) -> bool {
        self.mnt_ns.contains_mountpoint(mountpoint)
    }

    /// Returns a reference to the root directory.
    pub(crate) fn root_dir(&self) -> &Location {
        &self.root_dir
    }

    /// Returns a reference to the current working directory.
    #[cfg(feature = "vfs")]
    pub(crate) fn current_dir(&self) -> &Location {
        &self.current_dir
    }

    pub(super) fn root_location_active(&self) -> Location {
        self.root_dir.clone()
    }

    pub(super) fn current_location_active(&self) -> Location {
        self.current_dir.clone()
    }

    /// Changes the current working directory using its original authority.
    pub fn set_current_dir(&mut self, current_dir: FileLocation) -> VfsResult<()> {
        let operation = self.begin_operation()?;
        let current_dir = self.import_file_location(current_dir, operation.as_ref())?;
        self.set_current_dir_active(current_dir)
    }

    pub(crate) fn set_current_dir_during(
        &mut self,
        current_dir: Location,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<()> {
        self.validate_operation(operation)?;
        self.set_current_dir_active(current_dir)
    }

    fn set_current_dir_active(&mut self, current_dir: Location) -> VfsResult<()> {
        self.validate_location_namespace(&current_dir)?;
        current_dir.check_is_dir()?;
        self.current_dir = current_dir;
        Ok(())
    }

    /// Replaces root and current directory using their original authority.
    pub fn reset_root(&mut self, root_dir: FileLocation) -> VfsResult<()> {
        let operation = self.begin_operation()?;
        let root_dir = self.import_file_location(root_dir, operation.as_ref())?;
        self.reset_root_active(root_dir)
    }

    fn reset_root_active(&mut self, root_dir: Location) -> VfsResult<()> {
        self.validate_location_namespace(&root_dir)?;
        root_dir.check_is_dir()?;
        self.root_dir = root_dir.clone();
        self.current_dir = root_dir;
        Ok(())
    }

    fn clone_with_current_dir(&self, current_dir: Location) -> VfsResult<Self> {
        self.validate_location_namespace(&current_dir)?;
        current_dir.check_is_dir()?;
        Ok(Self {
            root_dir: self.root_dir.clone(),
            current_dir,
            #[cfg(feature = "vfs")]
            mnt_ns: self.mnt_ns.clone(),
            lifecycle: self.lifecycle.clone(),
        })
    }

    fn import_file_location(
        &self,
        location: FileLocation,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<Location> {
        let location = match (&self.lifecycle, operation, location) {
            (Some(_), Some(operation), FileLocation::Managed(location)) => {
                location
                    .validate_operation(operation)
                    .map_err(map_lifecycle_error)?;
                location.into_parts().0
            }
            (None, None, FileLocation::Unmanaged(location)) => location.into_inner(),
            _ => return Err(VfsError::BadState),
        };
        self.validate_location_namespace(&location)?;
        Ok(location)
    }

    fn validate_location_namespace(&self, location: &Location) -> VfsResult<()> {
        #[cfg(feature = "vfs")]
        if !self.mount_namespace_contains(location.mountpoint()) {
            return Err(VfsError::BadState);
        }
        #[cfg(not(feature = "vfs"))]
        let _ = location;
        Ok(())
    }

    /// Rebind this context to a freshly cloned mount namespace.
    #[cfg(feature = "vfs")]
    pub fn unshare_mount_namespace(&mut self) -> VfsResult<()> {
        let operation = self.begin_operation()?;
        let new_ns = self.mnt_ns.clone_namespace();
        self.set_mount_namespace_active(new_ns, operation.as_ref())
    }

    /// Rebind this context to an existing mount namespace.
    #[cfg(feature = "vfs")]
    pub fn set_mount_namespace(&mut self, new_ns: Arc<MountNamespace>) -> VfsResult<()> {
        let operation = self.begin_operation()?;
        self.set_mount_namespace_active(new_ns, operation.as_ref())
    }

    #[cfg(feature = "vfs")]
    fn set_mount_namespace_active(
        &mut self,
        new_ns: Arc<MountNamespace>,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<()> {
        new_ns
            .validate_operation(operation)
            .map_err(map_lifecycle_error)?;
        let root_path = self.root_dir.absolute_path()?;
        let current_path = self.current_dir.absolute_path()?;
        let new_root_loc = new_ns.root_mount().root_location();
        let resolver = Self::new_in_namespace(new_ns.clone(), new_root_loc, self.lifecycle.clone());
        let root_dir = resolver.resolve_active(root_path)?;
        let current_dir = resolver.resolve_active(current_path)?;
        self.mnt_ns = new_ns;
        self.root_dir = root_dir;
        self.current_dir = current_dir;
        Ok(())
    }

    fn try_resolve_symlink_active(
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
        self.resolve_components_active(PathBuf::from(target).components(), follow_count)
    }

    fn lookup(&self, dir: &Location, name: &str, follow_count: &mut usize) -> VfsResult<Location> {
        let loc = dir.lookup_no_follow(name)?;
        self.clone_with_current_dir(dir.clone())?
            .try_resolve_symlink_active(loc, follow_count)
    }

    fn resolve_components_active(
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
        let dir = self.resolve_components_active(components, follow_count)?;
        dir.check_is_dir()?;
        Ok((dir, entry_name))
    }

    /// Resolves a path starting from `current_dir`.
    pub(crate) fn resolve(&self, path: impl AsRef<Path>) -> VfsResult<Location> {
        let _operation = self.begin_operation()?;
        self.resolve_active(path)
    }

    pub(super) fn resolve_active(&self, path: impl AsRef<Path>) -> VfsResult<Location> {
        let mut follow_count = 0;
        let (dir, name) = self.resolve_inner(path.as_ref(), &mut follow_count)?;
        match name {
            Some(name) => self.lookup(&dir, name, &mut follow_count),
            None => Ok(dir),
        }
    }

    pub(super) fn resolve_no_follow_active(&self, path: impl AsRef<Path>) -> VfsResult<Location> {
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
    pub(super) fn resolve_parent_active<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(Location, Cow<'a, str>)> {
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
    pub(super) fn resolve_nonexistent_active<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(Location, &'a str)> {
        let (dir, name) = self.resolve_inner(path, &mut 0)?;
        if let Some(name) = name {
            Ok((dir, name))
        } else {
            Err(VfsError::InvalidInput)
        }
    }

    /// Retrieves metadata for the file.
    pub fn metadata(&self, path: impl AsRef<Path>) -> VfsResult<Metadata> {
        let operation = self.begin_operation()?;
        self.metadata_during(path.as_ref(), operation.as_ref())
    }

    pub(super) fn metadata_during(
        &self,
        path: &Path,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<Metadata> {
        self.validate_operation(operation)?;
        self.resolve_active(path)?.metadata()
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
        let lease = self.open_handle()?;
        let dir = self.resolve(path)?;
        Ok(ReadDir::new(dir, lease))
    }

    /// Removes a file from the filesystem.
    pub fn remove_file(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        let operation = self.begin_operation()?;
        self.remove_file_during(path.as_ref(), operation.as_ref())
    }

    pub(super) fn remove_file_during(
        &self,
        path: &Path,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<()> {
        self.validate_operation(operation)?;
        let entry = self.resolve_no_follow_active(path)?;
        entry
            .parent()
            .ok_or(VfsError::IsADirectory)?
            .unlink(&entry.name(), false)
    }

    /// Removes a directory from the filesystem.
    pub fn remove_dir(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        let operation = self.begin_operation()?;
        self.remove_dir_during(path.as_ref(), operation.as_ref())
    }

    pub(super) fn remove_dir_during(
        &self,
        path: &Path,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<()> {
        self.validate_operation(operation)?;
        let entry = self.resolve_no_follow_active(path)?;
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
        let _operation = self.begin_operation()?;
        let (src_dir, src_name) = self.resolve_parent_active(from.as_ref())?;
        let (dst_dir, dst_name) = self.resolve_parent_active(to.as_ref())?;
        src_dir.rename(&src_name, &dst_dir, &dst_name)
    }

    /// Creates a new, empty directory at the provided path.
    pub fn create_dir(
        &self,
        path: impl AsRef<Path>,
        mode: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileLocation> {
        let operation = self.begin_operation()?;
        self.create_dir_during(path.as_ref(), mode, uid, gid, operation.as_ref())
    }

    pub(super) fn create_dir_during(
        &self,
        path: &Path,
        mode: NodePermission,
        uid: u32,
        gid: u32,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<FileLocation> {
        self.validate_operation(operation)?;
        if path.as_str().is_empty() {
            return Err(VfsError::NotFound);
        }
        let (dir, name) = match self.resolve_nonexistent_active(path) {
            Ok(pair) => pair,
            Err(VfsError::InvalidInput) => {
                return match self.resolve_active(path) {
                    Ok(loc) if loc.node_type() == NodeType::Directory => {
                        Err(VfsError::AlreadyExists)
                    }
                    Ok(_) => Err(VfsError::NotADirectory),
                    Err(e) => Err(e),
                };
            }
            Err(e) => return Err(e),
        };
        let location = dir.create(name, NodeType::Directory, mode, uid, gid)?;
        self.bind_file_location(location, operation)
    }

    /// Creates a new hard link on the filesystem.
    pub fn link(
        &self,
        old_path: impl AsRef<Path>,
        new_path: impl AsRef<Path>,
    ) -> VfsResult<FileLocation> {
        let operation = self.begin_operation()?;
        let old = self.resolve_active(old_path.as_ref())?;
        let (new_dir, new_name) = self.resolve_nonexistent_active(new_path.as_ref())?;
        let location = new_dir.link(new_name, &old)?;
        self.bind_file_location(location, operation.as_ref())
    }

    /// Creates a new symbolic link on the filesystem.
    pub fn symlink(
        &self,
        target: impl AsRef<str>,
        link_path: impl AsRef<Path>,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileLocation> {
        let operation = self.begin_operation()?;
        self.symlink_during(
            target.as_ref(),
            link_path.as_ref(),
            uid,
            gid,
            operation.as_ref(),
        )
    }

    pub(super) fn symlink_during(
        &self,
        target: &str,
        link_path: &Path,
        uid: u32,
        gid: u32,
        operation: Option<&FsOperationLease>,
    ) -> VfsResult<FileLocation> {
        self.validate_operation(operation)?;
        let (dir, name) = self.resolve_nonexistent_active(link_path)?;
        if dir.lookup_no_follow(name).is_ok() {
            return Err(VfsError::AlreadyExists);
        }
        let symlink = dir.create(name, NodeType::Symlink, NodePermission::default(), uid, gid)?;
        symlink.entry().as_file()?.set_symlink(target)?;
        self.bind_file_location(symlink, operation)
    }

    /// Returns the canonical, absolute form of a path.
    pub fn canonicalize(&self, path: impl AsRef<Path>) -> VfsResult<PathBuf> {
        let _operation = self.begin_operation()?;
        self.resolve_active(path.as_ref())?.absolute_path()
    }

    /// Pivots to `new_root`, attaches the old root at `put_old`, and returns an
    /// opaque transition for task-wide root propagation.
    pub fn pivot_root_paths(
        &mut self,
        new_root: impl AsRef<Path>,
        put_old: impl AsRef<Path>,
    ) -> VfsResult<PivotRootTransition> {
        let operation = self.begin_operation()?;
        if !self.root_dir.is_root_of_mount() {
            return Err(VfsError::InvalidInput);
        }
        let new_root = self.resolve_active(new_root)?;
        let put_old = self.resolve_active(put_old)?;
        new_root.check_is_dir()?;
        put_old.check_is_dir()?;
        if !(new_root.is_root_of_mount() && !new_root.mountpoint().is_root()) {
            return Err(VfsError::InvalidInput);
        }

        let old_root = self.root_dir.clone();
        let old_root_mp = self.root_dir.mountpoint().clone();
        let new_root_mp = new_root.mountpoint().clone();
        old_root_mp.pivot_mount(&new_root_mp, &put_old)?;
        let new_root_loc = new_root_mp.root_location();
        self.root_dir = new_root_loc.clone();
        // Only replace cwd if it was pointing at the old root — mirrors
        // Linux's chroot_fs_refs / replace_path semantics.
        if old_root.ptr_eq(&self.current_dir) {
            self.current_dir = new_root_loc.clone();
        }
        Ok(PivotRootTransition {
            old_root: self.bind_file_location(old_root, operation.as_ref())?,
            new_root: self.bind_file_location(new_root_loc, operation.as_ref())?,
        })
    }

    /// Propagates a completed pivot to every task in the same mount namespace.
    ///
    /// This mirrors `chroot_fs_refs()` in Linux's `fs/namespace.c`:
    /// after `pivot_root(2)` reorganises the mount tree the kernel walks
    /// every thread's `fs_struct` and switches any `root` / `pwd` that
    /// pointed at the old root over to the new root.
    ///
    /// The operation is rejected if the source generation has frozen or the
    /// transition is stale after remount.
    pub fn propagate_pivot_root(transition: &PivotRootTransition) -> VfsResult<()> {
        transition.with_locations(|old_root, new_root| {
            // Collect strong references while holding the registry lock, then
            // release it so we never nest two PI mutex guards.
            let refs = registered_contexts();

            // Walk every live FsContext and apply the same logic as Linux
            // chroot_fs_refs().
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
            Ok(())
        })
    }
}

impl PivotRootTransition {
    fn with_locations<T>(
        &self,
        operation: impl FnOnce(&Location, &Location) -> VfsResult<T>,
    ) -> VfsResult<T> {
        match (&self.old_root, &self.new_root) {
            (FileLocation::Managed(old_root), FileLocation::Managed(new_root)) => {
                let operation_lease = old_root.begin_operation().map_err(map_lifecycle_error)?;
                new_root
                    .validate_operation(&operation_lease)
                    .map_err(map_lifecycle_error)?;
                operation(old_root.location(), new_root.location())
            }
            (FileLocation::Unmanaged(old_root), FileLocation::Unmanaged(new_root)) => {
                operation(old_root.as_inner(), new_root.as_inner())
            }
            _ => Err(VfsError::BadState),
        }
    }
}

fn map_lifecycle_error(error: FsRuntimeError) -> VfsError {
    error.into_ax_error()
}
