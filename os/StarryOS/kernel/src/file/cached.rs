//! Nonblocking descriptor selection and publication for cache-only opens.

use alloc::sync::Arc;
use core::ffi::c_int;

use axfs_ng_vfs::PathHandle;
use scope_local::Scope;

use super::{Directory, FD_TABLE, File, FileDescriptor, FileLike, FileTable};
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

    pub(crate) fn install(&self, file: Arc<dyn FileLike>, cloexec: bool) -> StarryResult<c_int> {
        let mut table = self.table.try_write().ok_or(StarryError::WouldBlock)?;
        let fd = table.reserve().ok_or(StarryError::TooManyOpenFiles)?;
        if fd as u64 >= self.limit {
            table.release_reserved(fd);
            return Err(StarryError::TooManyOpenFiles);
        }
        // Inline reservation and installation share this one guard, so no
        // close or replacement can consume the reservation between them.
        let installed = table.install_reserved(
            fd,
            FileDescriptor {
                inner: file.clone(),
                cloexec,
            },
        );
        assert!(installed.is_ok(), "cache-only fd reservation disappeared");
        // The parameter retains the file through unlock, including failures.
        drop(table);
        Ok(fd as c_int)
    }
}
