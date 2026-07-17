use alloc::string::String;
use core::fmt;

use axfs_ng_vfs::{FileNode, FilesystemDetachPolicy, Location, NodeType, Reference, VfsResult};

use super::operation::LocationOperationView;
use crate::lifecycle::{FsGenerationAccess, FsOpenHandleLease, FsOperationLease, FsRuntimeError};

/// A location owned by a filesystem that cannot participate in root handoff.
///
/// This capability is the only public route to construct an ax-fs-ng handle
/// without a filesystem-generation lease. It is intended for kernel-owned
/// synthetic filesystems such as tmpfs, devfs, and procfs.
#[derive(Clone, Debug)]
pub struct UnmanagedLocation {
    location: Location,
}

/// A location tied to one mounted filesystem generation.
///
/// Values can only be minted from a generation lease or from a managed
/// [`crate::FsContext`] resolution. Holding this capability does not delay a
/// freeze; each operation must still acquire an [`FsOperationLease`].
#[derive(Clone)]
pub struct GenerationBoundLocation {
    location: Location,
    access: FsGenerationAccess,
}

/// A location carrying the authority needed to use it after path resolution.
///
/// The variants prevent a raw [`Location`] from being relabelled as belonging
/// to whichever filesystem context happens to consume it.
#[derive(Clone, Debug)]
pub enum FileLocation {
    /// Location owned by a detachable, generation-managed filesystem.
    Managed(GenerationBoundLocation),
    /// Location owned by a checked non-detachable synthetic filesystem.
    Unmanaged(UnmanagedLocation),
}

/// Error returned when a detachable location is used as unmanaged storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum UnmanagedLocationError {
    /// The location belongs to a filesystem that participates in handoff.
    #[error("location belongs to a detachable filesystem")]
    DetachableFilesystem,
    /// Synthetic namespaces cannot make controller-backed block nodes unmanaged.
    #[error("block-device nodes require generation-managed access")]
    ExternalBlockDevice,
}

impl UnmanagedLocation {
    /// Validates and wraps a non-detachable synthetic filesystem location.
    ///
    /// # Errors
    ///
    /// Returns [`UnmanagedLocationError::DetachableFilesystem`] unless the
    /// location's filesystem explicitly declares itself non-detachable.
    /// Returns [`UnmanagedLocationError::ExternalBlockDevice`] for block nodes
    /// even when they are published inside a synthetic namespace.
    pub fn try_new(location: Location) -> Result<Self, UnmanagedLocationError> {
        if location.filesystem().detach_policy() != FilesystemDetachPolicy::NonDetachable {
            return Err(UnmanagedLocationError::DetachableFilesystem);
        }
        if location.node_type() == NodeType::BlockDevice {
            return Err(UnmanagedLocationError::ExternalBlockDevice);
        }
        Ok(Self { location })
    }

    pub(crate) fn into_inner(self) -> Location {
        self.location
    }

    pub(crate) fn as_inner(&self) -> &Location {
        &self.location
    }

    /// Creates an unattached synthetic file beside this proven unmanaged
    /// location.
    ///
    /// This supports kernel-only device objects such as a newly allocated PTY
    /// master. The resulting location is revalidated before it is returned.
    pub fn synthetic_sibling_file(
        &self,
        node: FileNode,
        node_type: NodeType,
        name: String,
    ) -> Result<Self, UnmanagedLocationError> {
        let entry = axfs_ng_vfs::DirEntry::new_file(
            node,
            node_type,
            Reference::new(Some(self.location.entry().clone()), name),
        );
        Self::try_new(Location::new(self.location.mountpoint().clone(), entry))
    }
}

impl GenerationBoundLocation {
    pub(crate) fn from_handle(location: Location, lease: &FsOpenHandleLease) -> Self {
        Self {
            location,
            access: lease.generation_access(),
        }
    }

    pub(crate) fn from_access(location: Location, access: FsGenerationAccess) -> Self {
        Self { location, access }
    }

    pub(super) fn from_operation(location: Location, operation: &FsOperationLease) -> Self {
        Self {
            location,
            access: operation.generation_access(),
        }
    }

    pub(crate) fn location(&self) -> &Location {
        &self.location
    }

    /// Verifies that the generation is still mounted.
    pub fn validate_generation(&self) -> Result<(), FsRuntimeError> {
        self.access.validate()
    }

    pub(crate) fn begin_operation(&self) -> Result<FsOperationLease, FsRuntimeError> {
        self.access.begin_operation()
    }

    pub(crate) fn validate_operation(
        &self,
        operation: &FsOperationLease,
    ) -> Result<(), FsRuntimeError> {
        self.access.validate_operation(operation)
    }

    pub(crate) fn into_parts(self) -> (Location, FsGenerationAccess) {
        (self.location, self.access)
    }
}

impl fmt::Debug for GenerationBoundLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GenerationBoundLocation")
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

impl FileLocation {
    /// Runs one restricted operation without exposing the underlying location.
    pub fn with_operation<T>(
        &self,
        operation: impl for<'operation> FnOnce(LocationOperationView<'operation>) -> VfsResult<T>,
    ) -> VfsResult<T> {
        match self {
            Self::Managed(location) => {
                let operation_lease = location
                    .begin_operation()
                    .map_err(FsRuntimeError::into_ax_error)?;
                operation(LocationOperationView::managed(
                    location.location(),
                    &operation_lease,
                ))
            }
            Self::Unmanaged(location) => {
                operation(LocationOperationView::unmanaged(location.as_inner()))
            }
        }
    }

    /// Verifies that the managed generation is still mounted.
    pub fn validate_generation(&self) -> Result<(), FsRuntimeError> {
        match self {
            Self::Managed(location) => location.validate_generation(),
            Self::Unmanaged(_) => Ok(()),
        }
    }
}

impl TryFrom<Location> for UnmanagedLocation {
    type Error = UnmanagedLocationError;

    fn try_from(location: Location) -> Result<Self, Self::Error> {
        Self::try_new(location)
    }
}
