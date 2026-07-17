//! Generation-scoped path and mount-namespace operations.

use alloc::borrow::Cow;

use axfs_ng_vfs::{Location, Metadata, NodePermission, VfsResult, path::Path};

use super::state::FsContext;
use crate::{
    file::{FileLocation, LocationOperationView, OpenOptions, OpenResult},
    lifecycle::FsOperationLease,
};

/// A non-escaping filesystem-context view bound to one admitted operation.
///
/// Composite path operations must use this view instead of calling ordinary
/// [`FsContext`] methods, which deliberately start independent operations.
/// Its private references and higher-ranked constructors keep the view inside
/// the lifetime of the exact generation lease.
pub struct FsContextOperationView<'operation> {
    context: &'operation FsContext,
    operation: Option<&'operation FsOperationLease>,
}

/// One admitted operation over an [`FsContext`] namespace.
///
/// The view borrows the exact generation lease acquired before path
/// resolution. Resolved locations therefore cannot escape the callback, while
/// operations admitted before a freeze can finish under the same lease.
pub struct FsNamespaceOperationView<'operation> {
    context: &'operation FsContext,
    operation: Option<&'operation FsOperationLease>,
}

impl<'operation> FsContextOperationView<'operation> {
    pub(crate) fn new(
        context: &'operation FsContext,
        operation: Option<&'operation FsOperationLease>,
    ) -> VfsResult<Self> {
        context.validate_operation(operation)?;
        Ok(Self { context, operation })
    }

    /// Runs namespace resolution without admitting a nested operation.
    pub fn with_namespace_operation<T>(
        &self,
        operation: impl for<'view> FnOnce(FsNamespaceOperationView<'view>) -> VfsResult<T>,
    ) -> VfsResult<T> {
        self.context.validate_operation(self.operation)?;
        operation(FsNamespaceOperationView::new(self.context, self.operation))
    }

    /// Resolves a path and retains generation authority for later use.
    pub fn resolve_file_location(&self, path: impl AsRef<Path>) -> VfsResult<FileLocation> {
        self.context
            .resolve_file_location_during(path.as_ref(), self.operation)
    }

    /// Resolves without following the final symlink and retains the result.
    pub fn resolve_file_location_no_follow(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<FileLocation> {
        self.context
            .resolve_file_location_no_follow_during(path.as_ref(), self.operation)
    }

    /// Resolves a parent directory and the final path component.
    pub fn resolve_parent_file_location<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(FileLocation, alloc::borrow::Cow<'a, str>)> {
        self.context
            .resolve_parent_file_location_during(path, self.operation)
    }

    /// Resolves the parent of an entry that will be created.
    pub fn resolve_nonexistent_file_location<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(FileLocation, &'a str)> {
        self.context
            .resolve_nonexistent_file_location_during(path, self.operation)
    }

    /// Retrieves metadata relative to this scoped context.
    pub fn metadata(&self, path: impl AsRef<Path>) -> VfsResult<Metadata> {
        self.context.metadata_during(path.as_ref(), self.operation)
    }

    /// Opens a file or directory while reusing the admitted operation.
    pub fn open(&self, options: &OpenOptions, path: impl AsRef<Path>) -> VfsResult<OpenResult> {
        options.open_scoped(self.context, path.as_ref(), self.operation)
    }

    /// Creates an empty directory relative to this scoped context.
    pub fn create_dir(
        &self,
        path: impl AsRef<Path>,
        mode: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileLocation> {
        self.context
            .create_dir_during(path.as_ref(), mode, uid, gid, self.operation)
    }

    /// Removes one non-directory entry relative to this scoped context.
    pub fn remove_file(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        self.context
            .remove_file_during(path.as_ref(), self.operation)
    }

    /// Removes one empty directory relative to this scoped context.
    pub fn remove_dir(&self, path: impl AsRef<Path>) -> VfsResult<()> {
        self.context
            .remove_dir_during(path.as_ref(), self.operation)
    }

    /// Creates a symbolic link relative to this scoped context.
    pub fn symlink(
        &self,
        target: impl AsRef<str>,
        link_path: impl AsRef<Path>,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileLocation> {
        self.context.symlink_during(
            target.as_ref(),
            link_path.as_ref(),
            uid,
            gid,
            self.operation,
        )
    }
}

impl<'operation> FsNamespaceOperationView<'operation> {
    pub(super) const fn new(
        context: &'operation FsContext,
        operation: Option<&'operation FsOperationLease>,
    ) -> Self {
        Self { context, operation }
    }

    fn authorize(&self, location: Location) -> LocationOperationView<'operation> {
        match self.operation {
            Some(operation) => LocationOperationView::managed_owned(location, operation),
            None => LocationOperationView::unmanaged_owned(location),
        }
    }

    /// Resolves a path while retaining the namespace operation lease.
    pub fn resolve_path(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<LocationOperationView<'operation>> {
        self.context
            .resolve_active(path)
            .map(|location| self.authorize(location))
    }

    /// Resolves a path without following its final symlink.
    pub fn resolve_path_no_follow(
        &self,
        path: impl AsRef<Path>,
    ) -> VfsResult<LocationOperationView<'operation>> {
        self.context
            .resolve_no_follow_active(path)
            .map(|location| self.authorize(location))
    }

    /// Resolves the parent directory and final path component.
    pub fn resolve_parent_path<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(LocationOperationView<'operation>, Cow<'a, str>)> {
        let (location, name) = self.context.resolve_parent_active(path)?;
        Ok((self.authorize(location), name))
    }

    /// Resolves the parent of a not-yet-created entry.
    pub fn parent_for_create<'a>(
        &self,
        path: &'a Path,
    ) -> VfsResult<(LocationOperationView<'operation>, &'a str)> {
        let (location, name) = self.context.resolve_nonexistent_active(path)?;
        Ok((self.authorize(location), name))
    }

    /// Returns the context root under the admitted operation.
    pub fn root(&self) -> LocationOperationView<'operation> {
        self.authorize(self.context.root_location_active())
    }

    /// Returns the current directory under the admitted operation.
    pub fn current_dir(&self) -> LocationOperationView<'operation> {
        self.authorize(self.context.current_location_active())
    }

    /// Resolves and retains a generation-safe location for later operations.
    pub fn retain(&self, path: impl AsRef<Path>) -> VfsResult<FileLocation> {
        let location = self.context.resolve_active(path)?;
        self.context.bind_file_location(location, self.operation)
    }

    /// Resolves without following the final symlink and retains the result.
    pub fn retain_no_follow(&self, path: impl AsRef<Path>) -> VfsResult<FileLocation> {
        let location = self.context.resolve_no_follow_active(path)?;
        self.context.bind_file_location(location, self.operation)
    }
}
