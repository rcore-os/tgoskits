//! Fixed file endpoints selected outside every admission and backend gate.

use smallvec::{SmallVec, smallvec};

use crate::{
    Location, MutationCredentials, NodePermission, PathHandle, PreparedWrite, VfsError, VfsResult,
};

/// One downward endpoint selection, retaining the original path and rights.
/// Factories bind the root. A layer may select only its declared strict backing;
/// it supplies its saved mounter, never the calling task or a root fallback.
pub struct FileIoRequest<'a> {
    paths: SmallVec<[PathHandle; 3]>,
    write: Option<&'a PreparedWrite>,
}

/// A fixed native endpoint plus all intermediate path uses for this operation.
/// It does not contain copied data, metadata, or a new authority registry.
pub struct FileIoTarget {
    paths: SmallVec<[PathHandle; 3]>,
}

impl<'a> FileIoRequest<'a> {
    /// Select a read endpoint for an already authorized path description.
    pub fn read(path: PathHandle) -> VfsResult<Self> {
        path.location().entry().as_file()?;
        if path.location().filesystem().stack_depth() > 2 {
            return Err(VfsError::InvalidInput);
        }
        Ok(Self {
            paths: smallvec![path],
            write: None,
        })
    }

    /// Borrow the exact original graph for one writable endpoint selection.
    /// Writable opens must prepare copy-up before publishing the description;
    /// selection never creates files or acquires another write preparation.
    pub fn write(path: PathHandle, write: &'a PreparedWrite) -> VfsResult<Self> {
        write
            .write_for(path.location())?
            .check_location(path.location())?;
        let mut request = Self::read(path)?;
        request.write = Some(write);
        Ok(request)
    }

    pub fn location(&self) -> &Location {
        self.paths.last().expect("bound file endpoint").location()
    }

    pub fn is_write(&self) -> bool {
        self.write.is_some()
    }

    /// Dispatch only to the actual selected receiver. No gate may be held.
    pub fn resolve(self) -> VfsResult<FileIoTarget> {
        let entry = self.location().entry().clone();
        entry.as_file()?.resolve_file_io(self)
    }

    /// A native owner's default completion. Composite owners must override the
    /// hook and forward; they cannot silently become their own native cache.
    pub fn native<N: crate::NodeOps + ?Sized>(self, owner: &N) -> VfsResult<FileIoTarget> {
        if !core::ptr::addr_eq(owner.filesystem(), self.location().filesystem()) {
            return Err(VfsError::CrossesDevices);
        }
        if owner.inode() != self.location().inode()
            || owner.identity().as_ref() != self.location().entry().identity()
        {
            return Err(VfsError::BadState);
        }
        if self.location().filesystem().stack_depth() != 0 {
            return Err(VfsError::OperationNotSupported);
        }
        Ok(FileIoTarget { paths: self.paths })
    }

    /// Follow one actual receiver's direct immutable dependency, checking the
    /// saved mounter before opening its endpoint. The composite privately proves
    /// the selected entity belongs to its bound source/upper, including unlink.
    pub fn forward_to(
        mut self,
        target: Location,
        mounter: &MutationCredentials<'_>,
    ) -> VfsResult<FileIoTarget> {
        let source = self.location().filesystem();
        if let Some(write) = self.write {
            write.write_for(&target)?.check_location(&target)?;
        }
        let declared = |root: &Location| {
            core::ptr::addr_eq(root.filesystem(), target.filesystem())
                && alloc::sync::Arc::ptr_eq(root.mountpoint(), target.mountpoint())
        };
        if core::ptr::addr_eq(source, target.filesystem())
            || source.stack_depth() > 2
            || target.filesystem().stack_depth() >= source.stack_depth()
            || !(source.write_backing_roots().iter().any(declared)
                || (self.write.is_none() && source.read_backing_roots().iter().any(declared)))
        {
            return Err(VfsError::BadState);
        }
        let access = if self.write.is_some() {
            NodePermission::OTHER_WRITE
        } else {
            NodePermission::OTHER_READ
        };
        mounter.check_permission(&target.metadata()?, access)?;
        self.paths.push(PathHandle::acquire(target)?);
        self.resolve()
    }
}

impl FileIoTarget {
    pub fn path_handle(&self) -> &PathHandle {
        self.paths.last().expect("native file endpoint")
    }
    pub fn location(&self) -> &Location {
        self.path_handle().location()
    }
    /// All actual identities selected by this operation, root to native leaf.
    pub fn paths(&self) -> &[PathHandle] {
        &self.paths
    }
}
