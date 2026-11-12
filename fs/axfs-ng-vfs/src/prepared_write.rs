//! Write leases and read pins for one immutable, bounded backing graph.

use alloc::{sync::Arc, vec::Vec};

use crate::{Location, MountWriteGuard, VfsError, VfsResult};

/// Prepares write authority and read dependencies before entering backend gates.
///
/// Backing descriptions must be immutable and nonblocking. Write leases are
/// acquired one mount at a time; no configuration mutex survives acquisition.
/// Read pins retain exact locations without granting write authority or blocking
/// unmount. Keep this object through every pressure, cancellation and explicit
/// finish, not merely until operation admissions exit. Failed preparation
/// releases all earlier leases and pins outside backend gates.
#[derive(Debug)]
pub struct PreparedWrite {
    backing: BackingPreparation,
}

#[derive(Debug, Default)]
struct BackingPreparation {
    writes: Vec<MountWriteGuard>,
    reads: Vec<Location>,
}

impl PreparedWrite {
    /// Prepares the visible mount for writing and all declared dependencies.
    ///
    /// Depth must not exceed two and must strictly decrease on every edge.
    /// Invalid declarations return [`VfsError::InvalidInput`] without modifying
    /// a backend. Read dependencies never acquire write leases, even for their
    /// own writable backing roots. A later write role also prepares that root's
    /// writable subtree. Write-admission errors are returned unchanged.
    pub fn acquire(location: &Location) -> VfsResult<Self> {
        let mut prepared = Self {
            backing: BackingPreparation::default(),
        };
        prepared
            .backing
            .acquire_backing(location, None, BackingRole::Write)?;
        Ok(prepared)
    }

    /// Borrows a lease already owned for exactly this mount.
    ///
    /// Does not perform admission or accept a writable mount alias in place of
    /// the requested mount. A read pin never supplies a lease. Missing authority
    /// returns `CrossesDevices`; callers must separately validate writable roots.
    pub fn write_for(&self, location: &Location) -> VfsResult<&MountWriteGuard> {
        self.backing.write_for(location)
    }
}

/// Owns creation preparation, retaining a write denial only after preparing the
/// entire dependency graph for reading. Keep it through all explicit finishes.
#[derive(Debug)]
pub struct PreparedCreation {
    state: CreationPreparation,
}

#[derive(Debug)]
enum CreationPreparation {
    Ready(PreparedWrite),
    Denied {
        error: VfsError,
        backing: BackingPreparation,
    },
}

impl PreparedCreation {
    /// Attempts complete write preparation before any backend admission. On
    /// failure all partial leases are released before preparing every dependency
    /// as read-only. A read-preparation error is returned unchanged.
    pub fn acquire(parent: &Location) -> VfsResult<Self> {
        let state = match PreparedWrite::acquire(parent) {
            Ok(write) => CreationPreparation::Ready(write),
            Err(error) => {
                let mut backing = BackingPreparation::default();
                backing.acquire_backing(parent, None, BackingRole::Read)?;
                CreationPreparation::Denied { error, backing }
            }
        };
        Ok(Self { state })
    }
}

/// A borrowed, crate-private view; it never owns or upgrades preparation.
#[derive(Clone, Copy)]
pub(crate) struct PreparedView<'a> {
    backing: &'a BackingPreparation,
    write: Option<&'a PreparedWrite>,
    denial: Option<VfsError>,
}

impl<'a> PreparedView<'a> {
    pub(crate) fn write(write: &'a PreparedWrite) -> Self {
        Self {
            backing: &write.backing,
            write: Some(write),
            denial: None,
        }
    }

    pub(crate) fn creation(creation: &'a PreparedCreation) -> Self {
        match &creation.state {
            CreationPreparation::Ready(write) => Self::write(write),
            CreationPreparation::Denied { error, backing } => Self {
                backing,
                write: None,
                denial: Some(*error),
            },
        }
    }

    pub(crate) fn filesystems(self) -> impl Iterator<Item = &'a dyn crate::FilesystemOps> {
        self.backing.filesystems()
    }

    pub(crate) fn require_write(self) -> VfsResult<&'a PreparedWrite> {
        self.write.ok_or(VfsError::CrossesDevices)
    }

    pub(crate) fn creation_write(self, target: &Location) -> VfsResult<crate::CreationWrite<'a>> {
        let root = target.mountpoint().root_location();
        if crate::namespace_admission::owner_address(root.filesystem())
            != crate::namespace_admission::owner_address(target.filesystem())
        {
            return Err(VfsError::CrossesDevices);
        }
        if self.backing.write_for(target).is_err()
            && !self
                .backing
                .reads
                .iter()
                .any(|read| Arc::ptr_eq(read.mountpoint(), target.mountpoint()))
        {
            return Err(VfsError::CrossesDevices);
        }
        if let Some(write) = self.write {
            // An existing-object selection may use a prepared read dependency.
            // Only CreationWrite::require may demand its real write lease.
            return Ok(crate::CreationWrite::Ready(write));
        }
        self.denial
            .map(crate::CreationWrite::Denied)
            .ok_or(VfsError::BadState)
    }
}

impl BackingPreparation {
    fn filesystems(&self) -> impl Iterator<Item = &dyn crate::FilesystemOps> {
        self.writes
            .iter()
            .map(MountWriteGuard::filesystem)
            .chain(self.reads.iter().map(Location::filesystem))
    }

    fn write_for(&self, location: &Location) -> VfsResult<&MountWriteGuard> {
        self.writes
            .iter()
            .find(|write| write.check_location(location).is_ok())
            .ok_or(VfsError::CrossesDevices)
    }

    fn acquire_backing(
        &mut self,
        location: &Location,
        parent_depth: Option<u32>,
        role: BackingRole,
    ) -> VfsResult<()> {
        let filesystem = location.filesystem();
        let depth = filesystem.stack_depth();
        if depth > 2 || parent_depth.is_some_and(|parent| depth >= parent) {
            return Err(VfsError::InvalidInput);
        }
        // Validate each edge before deduplication, including edges to an
        // already-held mount: a duplicate cannot hide a nondecreasing depth.
        if self.write_for(location).is_ok() {
            return Ok(());
        }
        match role {
            BackingRole::Read => {
                if self
                    .reads
                    .iter()
                    .any(|read| Arc::ptr_eq(read.mountpoint(), location.mountpoint()))
                {
                    return Ok(());
                }
                self.reads.try_reserve(1).map_err(|_| VfsError::NoMemory)?;
                self.reads.push(location.clone());
            }
            BackingRole::Write => {
                self.writes.try_reserve(1).map_err(|_| VfsError::NoMemory)?;
                self.writes.push(location.acquire_write()?);
                // An earlier read pin does not cover this root's write subtree.
            }
        }
        for backing in filesystem.read_backing_roots() {
            self.acquire_backing(backing, Some(depth), BackingRole::Read)?;
        }
        for backing in filesystem.write_backing_roots() {
            self.acquire_backing(backing, Some(depth), role)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum BackingRole {
    Read,
    Write,
}
