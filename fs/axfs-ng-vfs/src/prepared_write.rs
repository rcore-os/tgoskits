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
    writes: Vec<MountWriteGuard>,
    reads: Vec<Location>,
}

impl PreparedWrite {
    pub(crate) fn filesystems(&self) -> impl Iterator<Item = &dyn crate::FilesystemOps> {
        self.writes.iter().map(MountWriteGuard::filesystem)
    }

    /// Prepares the visible mount for writing and all declared dependencies.
    ///
    /// Depth must not exceed two and must strictly decrease on every edge.
    /// Invalid declarations return [`VfsError::InvalidInput`] without modifying
    /// a backend. Read dependencies never acquire write leases, even for their
    /// own writable backing roots. A later write role also prepares that root's
    /// writable subtree. Write-admission errors are returned unchanged.
    pub fn acquire(location: &Location) -> VfsResult<Self> {
        let mut prepared = Self {
            writes: Vec::new(),
            reads: Vec::new(),
        };
        prepared.acquire_backing(location, None, BackingRole::Write)?;
        Ok(prepared)
    }

    /// Borrows a lease already owned for exactly this mount.
    ///
    /// Does not perform admission or accept a writable mount alias in place of
    /// the requested mount. A read pin never supplies a lease. Missing authority
    /// returns `CrossesDevices`; callers must separately validate writable roots.
    pub fn write_for(&self, location: &Location) -> VfsResult<&MountWriteGuard> {
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
