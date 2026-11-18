//! Nonblocking descriptor selection and publication for cache-only opens.

use alloc::sync::Arc;
use core::ffi::c_int;

use axfs_ng_vfs::PathHandle;
use scope_local::Scope;

use super::{Directory, FD_TABLE, File, FileDescriptorReservation, FileTable};
use crate::{StarryError, StarryResult, sync::RawSpinRwLock};

/// The original table and fd-number limit, without a task-local resample.
pub(crate) struct CachedFileTable {
    table: Arc<RawSpinRwLock<FileTable>>,
    limit: u64,
}

impl CachedFileTable {
    pub(crate) fn from_scope(scope: &Scope, limit: u64) -> Self {
        let files = FD_TABLE.scope(scope);
        Self {
            table: files.table.clone(),
            limit,
        }
    }

    pub(crate) fn directory(&self, fd: c_int) -> StarryResult<PathHandle> {
        let file = {
            let table = self.table.try_read().ok_or(StarryError::WouldBlock)?;
            table
                .get(fd as usize)
                .ok_or(StarryError::BadFileDescriptor)?
                .inner
                .clone()
        };
        // The description remains owned until after the table guard ended.
        let path = if let Some(directory) = file.downcast_ref::<Directory>() {
            directory.path_handle()
        } else if let Some(file) = file.downcast_ref::<File>() {
            file.inner().path_handle()
        } else {
            return Err(StarryError::NotADirectory);
        };
        if !path.location().is_dir() {
            return Err(StarryError::NotADirectory);
        }
        Ok(path.clone())
    }

    pub(crate) fn reserve(&self) -> StarryResult<FileDescriptorReservation> {
        FileDescriptorReservation::try_in_table(self.table.clone(), self.limit)
    }
}
