//! Restricted operations over one generation-authorized file location.

use alloc::{string::String, sync::Arc};
use core::{any::Any, fmt, task::Context};

use axfs_ng_vfs::{
    DirEntry, DirEntrySink, Filesystem, FsIoEvents, FsPollable, Location, Metadata, MetadataUpdate,
    Mountpoint, NodeFlags, NodeOps, NodePermission, NodeType, StatFs, VfsError, VfsResult,
    path::PathBuf,
};

use super::location::{FileLocation, GenerationBoundLocation, UnmanagedLocation};
use crate::FsOperationLease;

/// A non-escaping view of a file location during one admitted operation.
///
/// The view intentionally exposes neither the raw [`Location`] nor any VFS
/// object from which it can be reconstructed. It is created only while the
/// owning file or backend retains its exact filesystem-generation operation
/// lease.
pub struct LocationOperationView<'operation> {
    location: OperationLocation<'operation>,
    _managed_lease: Option<&'operation FsOperationLease>,
}

enum OperationLocation<'operation> {
    Borrowed(&'operation Location),
    Owned(Location),
}

/// Opaque identity of one namespace-local mount.
///
/// The identity can be retained for equality and busy-state checks, but it
/// exposes neither its mountpoint nor a location. Mutating a mount still
/// requires an admitted [`LocationOperationView`].
#[derive(Clone)]
pub struct MountIdentity {
    mountpoint: Arc<Mountpoint>,
}

/// Linux-compatible propagation mode for one mountpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MountPropagation {
    Shared,
    Private,
    Slave,
    Unbindable,
}

impl OperationLocation<'_> {
    const fn as_ref(&self) -> &Location {
        match self {
            Self::Borrowed(location) => location,
            Self::Owned(location) => location,
        }
    }

    fn into_owned(self) -> Location {
        match self {
            Self::Borrowed(location) => location.clone(),
            Self::Owned(location) => location,
        }
    }
}

impl MountIdentity {
    pub(crate) fn new(mountpoint: Arc<Mountpoint>) -> Self {
        Self { mountpoint }
    }

    pub(crate) fn mountpoint(&self) -> &Arc<Mountpoint> {
        &self.mountpoint
    }
}

impl<'operation> LocationOperationView<'operation> {
    pub(super) const fn managed(
        location: &'operation Location,
        lease: &'operation FsOperationLease,
    ) -> Self {
        Self {
            location: OperationLocation::Borrowed(location),
            _managed_lease: Some(lease),
        }
    }

    pub(super) const fn unmanaged(location: &'operation Location) -> Self {
        Self {
            location: OperationLocation::Borrowed(location),
            _managed_lease: None,
        }
    }

    pub(crate) fn managed_owned(location: Location, lease: &'operation FsOperationLease) -> Self {
        Self {
            location: OperationLocation::Owned(location),
            _managed_lease: Some(lease),
        }
    }

    pub(crate) fn unmanaged_owned(location: Location) -> Self {
        Self {
            location: OperationLocation::Owned(location),
            _managed_lease: None,
        }
    }

    fn location(&self) -> &Location {
        self.location.as_ref()
    }

    fn validate_peer(&self, other: &Self) -> VfsResult<()> {
        match (self._managed_lease, other._managed_lease) {
            (Some(lease), Some(other_lease)) if lease.authorizes_same_generation(other_lease) => {
                Ok(())
            }
            (None, None) => Ok(()),
            _ => Err(VfsError::BadState),
        }
    }

    /// Retains this resolved location as a generation-safe capability.
    pub fn retain(self) -> VfsResult<FileLocation> {
        let Self {
            location,
            _managed_lease,
        } = self;
        let location = location.into_owned();
        match _managed_lease {
            Some(operation) => Ok(FileLocation::Managed(
                GenerationBoundLocation::from_operation(location, operation),
            )),
            None => UnmanagedLocation::try_new(location)
                .map(FileLocation::Unmanaged)
                .map_err(|_| VfsError::BadState),
        }
    }

    /// Authorizes another retained location with this exact operation lease.
    ///
    /// This supports composite operations such as cross-directory rename and
    /// overlay copy-up without admitting a nested operation after a freeze has
    /// begun. The retained location must belong to the same runtime and
    /// generation as this view.
    pub fn authorize_location<'view>(
        &'view self,
        location: &'view FileLocation,
    ) -> VfsResult<LocationOperationView<'view>> {
        match (&self._managed_lease, location) {
            (Some(operation), FileLocation::Managed(location)) => {
                location
                    .validate_operation(operation)
                    .map_err(|_| VfsError::BadState)?;
                Ok(LocationOperationView::managed(
                    location.location(),
                    operation,
                ))
            }
            (None, FileLocation::Unmanaged(location)) => {
                Ok(LocationOperationView::unmanaged(location.as_inner()))
            }
            _ => Err(VfsError::BadState),
        }
    }

    /// Returns metadata for the authorized file location.
    pub fn metadata(&self) -> VfsResult<Metadata> {
        self.location().metadata()
    }

    /// Returns the current file length.
    pub fn len(&self) -> VfsResult<u64> {
        self.location().len()
    }

    /// Returns whether the current file is empty.
    pub fn is_empty(&self) -> VfsResult<bool> {
        self.len().map(|len| len == 0)
    }

    /// Returns the node type without exposing its directory entry.
    pub fn node_type(&self) -> NodeType {
        self.location().node_type()
    }

    /// Returns whether this location names a directory.
    pub fn is_dir(&self) -> bool {
        self.location().is_dir()
    }

    /// Validates that this location names a directory.
    pub fn check_is_dir(&self) -> VfsResult<()> {
        self.location().check_is_dir()
    }

    /// Returns node behavior flags.
    pub fn node_flags(&self) -> NodeFlags {
        self.location().flags()
    }

    /// Returns whether the containing mount is read-only.
    pub fn is_readonly(&self) -> bool {
        self.location().is_readonly()
    }

    /// Returns the location's absolute namespace path as an owned value.
    pub fn absolute_path(&self) -> VfsResult<PathBuf> {
        self.location().absolute_path()
    }

    /// Returns the final path component as an owned value.
    pub fn name(&self) -> String {
        self.location().name().into_owned()
    }

    /// Returns the inode number without exposing the backing entry.
    pub fn inode(&self) -> u64 {
        self.location().inode()
    }

    /// Returns the namespace-local mount device identifier.
    pub fn mount_device(&self) -> u64 {
        self.location().mountpoint().device()
    }

    /// Tests whether this location belongs to `mountpoint`.
    pub fn is_on_mount(&self, mount: &MountIdentity) -> bool {
        Arc::ptr_eq(self.location().mountpoint(), mount.mountpoint())
    }

    /// Returns an opaque identity for the containing mount.
    pub fn mount_identity(&self) -> MountIdentity {
        MountIdentity::new(self.location().mountpoint().clone())
    }

    /// Returns whether this location is the root entry of its mount.
    pub fn is_mount_root(&self) -> bool {
        self.location().is_root_of_mount()
    }

    /// Returns whether this location belongs to the namespace root mount and
    /// names its root entry.
    pub fn is_namespace_root(&self) -> bool {
        self.location().is_root()
    }

    /// Changes this mount's propagation mode.
    pub fn set_mount_propagation(&self, propagation: MountPropagation) {
        let mountpoint = self.location().mountpoint();
        match propagation {
            MountPropagation::Shared => mountpoint.set_shared(),
            MountPropagation::Private => mountpoint.set_private(),
            MountPropagation::Slave => mountpoint.set_slave(),
            MountPropagation::Unbindable => mountpoint.set_unbindable(),
        }
    }

    /// Changes this mount's read-only flag.
    pub fn set_mount_readonly(&self, readonly: bool) {
        self.location().mountpoint().set_readonly(readonly);
    }

    /// Marks this mount expired and returns whether it was already expired.
    pub fn mark_mount_expired(&self) -> bool {
        self.location().mountpoint().mark_expired()
    }

    /// Mounts a filesystem at this path.
    pub fn mount_filesystem(
        &self,
        filesystem: &Filesystem,
        readonly: bool,
    ) -> VfsResult<MountIdentity> {
        let mountpoint = self.location().mount(filesystem)?;
        mountpoint.set_readonly(readonly);
        Ok(MountIdentity::new(mountpoint))
    }

    /// Creates a bind mount from `source` at this path.
    pub fn bind_mount(
        &self,
        source: &Self,
        recursive: bool,
        readonly: bool,
    ) -> VfsResult<MountIdentity> {
        self.validate_peer(source)?;
        let mountpoint = self.location().bind_mount(source.location(), recursive)?;
        mountpoint.set_readonly(readonly);
        Ok(MountIdentity::new(mountpoint))
    }

    /// Moves this mount to `target`.
    pub fn move_mount(&self, target: &Self) -> VfsResult<()> {
        self.validate_peer(target)?;
        self.location().move_mount(target.location())
    }

    /// Lazily detaches this mount.
    pub fn detach_mount(&self) -> VfsResult<()> {
        self.location().detach_mount()
    }

    /// Unmounts this mount after flushing its filesystem.
    pub fn unmount(&self) -> VfsResult<()> {
        self.location().unmount()
    }

    /// Recursively unmounts this mount tree.
    pub fn unmount_all(&self) -> VfsResult<()> {
        self.location().unmount_all()
    }

    /// Tests whether two admitted views name the same VFS location.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.location().ptr_eq(other.location())
    }

    /// Updates metadata while the exact generation operation remains admitted.
    pub fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        self.location().update_metadata(update)
    }

    /// Synchronizes file or directory data and optionally metadata.
    pub fn sync(&self, data_only: bool) -> VfsResult<()> {
        self.location().sync(data_only)
    }

    /// Flushes the containing filesystem without exposing its implementation.
    pub fn flush_filesystem(&self) -> VfsResult<()> {
        self.location().filesystem().flush()
    }

    /// Returns containing-filesystem statistics as an owned snapshot.
    pub fn filesystem_statistics(&self) -> VfsResult<StatFs> {
        self.location().filesystem().stat()
    }

    /// Returns the filesystem implementation name as an owned string.
    pub fn filesystem_name(&self) -> String {
        self.location().filesystem().name().into()
    }

    /// Dispatches a node ioctl under the admitted operation.
    pub fn ioctl(&self, command: u32, argument: usize) -> VfsResult<usize> {
        self.location().ioctl(command, argument)
    }

    /// Returns the node's current readiness snapshot.
    pub fn poll(&self) -> FsIoEvents {
        self.location().poll()
    }

    /// Registers an operation-scoped readiness waiter.
    pub fn register(&self, context: &mut Context<'_>, events: FsIoEvents) {
        self.location().register(context, events);
    }

    /// Reads directory entries starting at `offset`.
    pub fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        self.location().read_dir(offset, sink)
    }

    /// Reads file data directly under the admitted operation.
    pub fn read_at(&self, buffer: &mut [u8], offset: u64) -> VfsResult<usize> {
        self.location().entry().as_file()?.read_at(buffer, offset)
    }

    /// Writes file data directly under the admitted operation.
    pub fn write_at(&self, buffer: &[u8], offset: u64) -> VfsResult<usize> {
        self.location().entry().as_file()?.write_at(buffer, offset)
    }

    /// Appends file data under the admitted operation.
    pub fn append(&self, buffer: &[u8]) -> VfsResult<(usize, u64)> {
        self.location().entry().as_file()?.append(buffer)
    }

    /// Changes the file length under the admitted operation.
    pub fn set_len(&self, len: u64) -> VfsResult<()> {
        self.location().entry().as_file()?.set_len(len)
    }

    /// Changes a symbolic-link target under the admitted operation.
    pub fn set_symlink(&self, target: &str) -> VfsResult<()> {
        self.location().entry().as_file()?.set_symlink(target)
    }

    /// Reads a symbolic-link target as an owned string.
    pub fn read_link(&self) -> VfsResult<String> {
        self.location().read_link()
    }

    /// Creates a child and carries the same exact operation lease into the
    /// returned restricted view.
    pub fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<Self> {
        self.location()
            .create(name, node_type, permission, uid, gid)
            .map(|location| Self {
                location: OperationLocation::Owned(location),
                _managed_lease: self._managed_lease,
            })
    }

    /// Attaches an anonymous entry to this namespace location and proves that
    /// the resulting object belongs to a non-detachable synthetic filesystem.
    pub fn attach_unmanaged_entry(&self, entry: DirEntry) -> VfsResult<UnmanagedLocation> {
        UnmanagedLocation::try_new(Location::new(self.location().mountpoint().clone(), entry))
            .map_err(|_| VfsError::BadState)
    }

    /// Tests whether a direct child exists without exposing the resolved child.
    pub fn lookup_child_exists(&self, name: &str) -> VfsResult<bool> {
        match self.location().lookup_no_follow(name) {
            Ok(_) => Ok(true),
            Err(VfsError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Resolves a direct child without following its final symlink while
    /// retaining this exact operation lease.
    pub fn lookup_no_follow(&self, name: &str) -> VfsResult<Self> {
        self.location().lookup_no_follow(name).map(|location| Self {
            location: OperationLocation::Owned(location),
            _managed_lease: self._managed_lease,
        })
    }

    /// Creates a hard link to `source` in this directory.
    pub fn link(&self, name: &str, source: &Self) -> VfsResult<()> {
        self.validate_peer(source)?;
        self.location().link(name, source.location()).map(|_| ())
    }

    /// Creates a hard link and returns a restricted view of the new child.
    pub fn link_child(&self, name: &str, source: &Self) -> VfsResult<Self> {
        self.validate_peer(source)?;
        self.location()
            .link(name, source.location())
            .map(|location| Self {
                location: OperationLocation::Owned(location),
                _managed_lease: self._managed_lease,
            })
    }

    /// Removes a direct child.
    pub fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()> {
        self.location().unlink(name, is_dir)
    }

    /// Renames one child between two admitted directories.
    pub fn rename(
        &self,
        source_name: &str,
        destination: &Self,
        destination_name: &str,
    ) -> VfsResult<()> {
        self.validate_peer(destination)?;
        self.location()
            .rename(source_name, destination.location(), destination_name)
    }

    /// Runs an operation against a typed node without exposing its entry or an
    /// owning node reference.
    pub fn with_node<T, R>(
        &self,
        operation: impl for<'node> FnOnce(&'node T) -> VfsResult<R>,
    ) -> VfsResult<R>
    where
        T: NodeOps,
    {
        let node = self.location().entry().downcast::<T>()?;
        operation(node.as_ref())
    }

    /// Clones typed user data stored on the authorized file node.
    pub fn get_user_data<T>(&self) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        self.location().user_data().get::<T>()
    }

    /// Returns existing typed user data or inserts a newly constructed value.
    pub fn get_or_insert_user_data_with<T>(&self, create: impl FnOnce() -> T) -> Arc<T>
    where
        T: Any + Send + Sync,
    {
        self.location().user_data().get_or_insert_with(create)
    }

    /// Inserts typed node-attached data under the admitted operation.
    pub fn insert_user_data<T>(&self, value: T)
    where
        T: Any + Send + Sync,
    {
        self.location().user_data().insert(value);
    }
}

impl fmt::Debug for LocationOperationView<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocationOperationView")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for MountIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MountIdentity")
            .field("device", &self.mountpoint.device())
            .finish_non_exhaustive()
    }
}
