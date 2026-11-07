//! Native permission promotion without replacing a published filesystem owner.

use super::*;

impl Ext4Filesystem {
    /// Serializes against shutdown and drains reads before restoring writes.
    /// The caller publishes VFS write access only after this operation succeeds.
    pub(super) fn promote_read_write(&self) -> VfsResult<()> {
        let _lifecycle = self.lifecycle.lock();
        {
            // Check under the same lifecycle exclusion as shutdown, without
            // interrupting normal I/O for an already writable filesystem.
            let _cached_write = self.cached_writes.enter().map_err(into_vfs_err)?;
            let _operation = self.admission.enter().map_err(into_vfs_err)?;
            if !self.lock().ext4.options().readonly {
                return Ok(());
            }
        }
        let _cached_pause = self.cached_writes.pause_and_drain().map_err(into_vfs_err)?;
        let _operation_pause = self.admission.pause_and_drain().map_err(into_vfs_err)?;
        self.promote_quiescent()
    }

    /// Holds filesystem exclusion during the native remount, never during joins
    /// or worker startup. Read-only mounts retain synchronous writeback policy.
    fn promote_quiescent(&self) -> VfsResult<()> {
        let mut state = self.lock();
        let previous = state.ext4.options();
        if !previous.readonly {
            return Ok(());
        }
        let permissions = self
            .permissions
            .as_ref()
            .ok_or(VfsError::PermissionDenied)?;
        permissions
            .restore_read_write()
            .map_err(|error| match error {
                crate::BlockError::Unsupported => VfsError::PermissionDenied,
                error => crate::block_error_to_vfs_error(error),
            })?;
        let mut writable = previous;
        writable.readonly = false;
        if let Err(error) = state.ext4.remount(writable) {
            permissions
                .restrict_read_only()
                .map_err(crate::block_error_to_vfs_error)?;
            return Err(into_vfs_err(error));
        }
        let needs_mmp = state.ext4.mmp_refresh_interval().is_some();
        drop(state);
        if needs_mmp && let Err(error) = self.start_mmp_worker() {
            let rollback = self.lock().ext4.remount(previous);
            if let Err(cause) = rollback {
                // An uncertain rollback must never reopen mutation admission.
                self.cached_writes.fail(cause);
                self.lock().ext4.report_mmp_runtime_failure(cause);
                log::error!("ext4 remount rollback failed: {cause}");
            }
            permissions
                .restrict_read_only()
                .map_err(crate::block_error_to_vfs_error)?;
            return Err(error);
        }
        Ok(())
    }
}
