//! Optional cached-write lifetime tracking owned by a filesystem instance.

use crate::{FilesystemOps, VfsResult};

/// Counts cached mutations separately from internal filesystem writeback.
///
/// Callers use [`CachedWriteGuard`], not these callbacks directly. An
/// implementation serializes acquisition against closing and wakes drain
/// waiters after release, without retaining its state lock during notification.
pub trait CachedWriteAdmission: Send + Sync {
    /// Counts one operation, or returns an error without changing the count.
    fn acquire(&self) -> VfsResult<()>;

    /// Releases exactly one successful acquisition.
    fn release(&self);
}

/// Retains a filesystem's cached-write admission until a mutation completes.
/// Internal page writeback does not acquire this guard: shutdown must still
/// flush existing dirty pages after preventing new cached mutations.
#[must_use]
pub struct CachedWriteGuard<'a> {
    admission: Option<&'a dyn CachedWriteAdmission>,
}

impl<'a> CachedWriteGuard<'a> {
    /// Enters the optional filesystem boundary before taking file I/O locks.
    /// Returns the admission error unchanged if the filesystem is closing.
    pub fn acquire(filesystem: &'a dyn FilesystemOps) -> VfsResult<Self> {
        let admission = filesystem.cached_write_admission();
        if let Some(admission) = admission {
            admission.acquire()?;
        }
        Ok(Self { admission })
    }
}

impl core::fmt::Debug for CachedWriteGuard<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CachedWriteGuard")
            .field("tracked", &self.admission.is_some())
            .finish()
    }
}

impl Drop for CachedWriteGuard<'_> {
    fn drop(&mut self) {
        if let Some(admission) = self.admission {
            admission.release();
        }
    }
}
