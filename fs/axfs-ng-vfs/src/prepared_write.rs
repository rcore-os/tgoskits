//! Write leases for one operation and its immutable, bounded backing graph.

use alloc::vec::Vec;

use crate::{Location, MountWriteGuard, VfsError, VfsResult};

/// Owns every exact-mount write lease before any backend gate is entered.
///
/// Backing descriptions must be immutable and nonblocking. Admission is taken
/// one mount at a time; no configuration mutex survives an acquisition call.
/// Dropping a failed preparation releases all earlier leases outside gates.
#[derive(Debug)]
pub struct PreparedWrite {
    writes: Vec<MountWriteGuard>,
}

impl PreparedWrite {
    /// Prepares the visible mount and its declared writable backing roots.
    ///
    /// Depth must not exceed two and must strictly decrease on every edge.
    /// Invalid declarations return [`VfsError::InvalidInput`] without modifying
    /// a backend. Read-only and admission errors are returned unchanged.
    pub fn acquire(location: &Location) -> VfsResult<Self> {
        let mut prepared = Self { writes: Vec::new() };
        prepared.acquire_backing(location, None)?;
        Ok(prepared)
    }

    /// Borrows a lease already owned for exactly this mount.
    ///
    /// Does not perform admission or accept a writable mount alias in place of
    /// the requested mount. Missing authority returns `CrossesDevices`.
    pub fn write_for(&self, location: &Location) -> VfsResult<&MountWriteGuard> {
        self.writes
            .iter()
            .find(|write| write.check_location(location).is_ok())
            .ok_or(VfsError::CrossesDevices)
    }

    fn acquire_backing(&mut self, location: &Location, parent_depth: Option<u32>) -> VfsResult<()> {
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
        self.writes.try_reserve(1).map_err(|_| VfsError::NoMemory)?;
        self.writes.push(location.acquire_write()?);
        for backing in filesystem.write_backing_roots() {
            self.acquire_backing(backing, Some(depth))?;
        }
        Ok(())
    }
}
