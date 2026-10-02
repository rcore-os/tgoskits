//! Mount-local writeback policy. Device I/O never owns the ext4 state guard.

use alloc::sync::Arc;
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use rsext4::{Ext4ErrorKind, Ext4Result, PreparedCommit};

use super::*;
use crate::{
    block_error_to_vfs_error,
    os::{BlockNotification, runtime_ops},
};

const COMMIT_INTERVAL: Duration = Duration::from_secs(5);

/// Only commit owners take `gate`; ordinary inode operations do not. The
/// worker and explicit fsync callers execute the same owned-I/O state machine.
pub(super) struct Writeback {
    gate: Mutex<()>,
    enabled: bool,
    task: MountWorker,
}

#[derive(Clone, Copy)]
pub(super) enum CheckpointPolicy {
    LogPressure,
    Drain,
}

/// Releases staging admission on every exit, including allocation failures.
/// Only the commit-gate owner creates this guard.
struct StagingGuard<'a>(&'a Ext4Filesystem);

impl Drop for StagingGuard<'_> {
    fn drop(&mut self) {
        self.0.lock().staging = false;
    }
}

impl Writeback {
    pub(super) fn new(enabled: bool) -> Self {
        Self {
            gate: Mutex::new(()),
            enabled,
            task: MountWorker::disabled(),
        }
    }

    pub(super) fn stop_and_join(&self) {
        self.task.stop_and_join();
    }

    pub(super) fn stop(&self) {
        self.task.stop();
    }
}

impl Ext4Filesystem {
    pub(crate) fn background_writeback_enabled(&self) -> bool {
        self.writeback.enabled
    }

    pub(super) fn sync_core_for_reap(&self) -> Ext4Result<()> {
        self.sync_core(CheckpointPolicy::LogPressure)
    }

    pub(super) fn unmount_after_drain(&self) -> VfsResult<()> {
        let _commit = self.writeback.gate.lock();
        self.sync_with_commit_gate(CheckpointPolicy::Drain)
            .map_err(into_vfs_err)?;
        if !self.writeback.enabled {
            return self.lock().unmount();
        }
        let clean = {
            let mut state = self.lock();
            let clean = state.ext4.prepare_unmount().map_err(into_vfs_err)?;
            state.shutdown_attempted = true;
            clean
        };
        let receipt = clean.execute();
        let result = self.lock().ext4.finish_unmount(&receipt);
        if let Err(error) = result {
            log::error!("ext4 final clean publication failed: {error}; {receipt:?}");
        }
        result.map_err(into_vfs_err)
    }

    pub(super) fn configure_writeback(ext4: &mut MountedExt4) -> VfsResult<bool> {
        ext4.use_shared_device_cache().map_err(into_vfs_err)?;
        if ext4.options().readonly {
            return Ok(false);
        }
        if !crate::os::has_runtime_ops() {
            log::info!("ext4 uses synchronous writeback: task runtime unavailable");
            return Ok(false);
        }
        let runtime = runtime_ops().map_err(block_error_to_vfs_error)?;
        if !runtime.can_block() {
            log::error!("ext4 background writeback requires a blocking task context");
            return Err(VfsError::WouldBlock);
        }
        match ext4.enable_background_writeback() {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == Ext4ErrorKind::UnsupportedCapability => {
                log::info!("ext4 uses synchronous writeback: {error}");
                Ok(false)
            }
            Err(error) => {
                log::error!("ext4 writeback initialization failed: {error}");
                Err(into_vfs_err(error))
            }
        }
    }

    pub(crate) fn sync_to_disk(&self) -> VfsResult<()> {
        let _operation = self.admission.enter().map_err(into_vfs_err)?;
        self.sync_core(CheckpointPolicy::LogPressure)
            .map_err(into_vfs_err)
    }

    pub(super) fn sync_core(&self, checkpoint: CheckpointPolicy) -> Ext4Result<()> {
        let _commit = self.writeback.gate.lock();
        self.sync_with_commit_gate(checkpoint)
    }

    pub(super) fn persist_latched_abort(&self) {
        if !self.writeback.enabled {
            return;
        }
        let _commit = self.writeback.gate.lock();
        let pending = self.lock().ext4.prepare_writeback_abort();
        match pending {
            Ok(Some(batch)) => {
                // execute_writeback logs the original cause and the separate
                // persistence error; the failed caller retains its own cause.
                let _result = self.execute_writeback(batch);
            }
            Ok(None) => {}
            Err(error) => {
                log::error!("ext4 abort preparation failed: {error}");
            }
        }
    }

    pub(super) fn sync_with_commit_gate(&self, checkpoint: CheckpointPolicy) -> Ext4Result<()> {
        if !self.writeback.enabled {
            return self.lock().ext4.sync();
        }
        let abort = self.lock().ext4.prepare_writeback_abort()?;
        if let Some(batch) = abort {
            return self.execute_writeback(batch);
        }
        // A previous attempt may have drained the commit but failed to obtain
        // the checkpoint endpoint. Finish that owner before staging metadata;
        // otherwise the closed admission gate would make staging retry itself.
        if self.lock().ext4.writeback_checkpoint_pending() {
            self.checkpoint_with_commit_gate()?;
        }
        let _staging = StagingGuard(self);
        loop {
            let pre_read = self.lock().ext4.prepare_inode_table_read()?;
            let completed = pre_read.map(|read| read.execute()).transpose()?;
            let (batch, needs_checkpoint, retry_staging) = {
                let mut state = self.lock();
                let needs_checkpoint = matches!(checkpoint, CheckpointPolicy::Drain)
                    || state.ext4.writeback_checkpoint_needed()?;
                let prepared = completed
                    .as_ref()
                    .map_or(Ok(()), |read| state.ext4.stage_inode_table_read(read))
                    .and_then(|()| {
                        if needs_checkpoint {
                            state.ext4.prepare_sync_for_checkpoint()
                        } else {
                            state.ext4.prepare_sync()
                        }
                    });
                let (batch, retry_staging) = match prepared {
                    Ok(batch) => (batch, false),
                    Err(error) if error.requires_journal_progress() => {
                        let batch = if needs_checkpoint {
                            state.ext4.prepare_writeback_progress_for_checkpoint()?
                        } else {
                            state.ext4.prepare_writeback_progress()?
                        };
                        (batch, true)
                    }
                    Err(error) => return Err(error),
                };
                state.dirty = retry_staging;
                // An fsync whose cached metadata spans multiple transactions
                // must finish a finite captured prefix. Pause new mutations
                // only until the remaining metadata is sealed, never while
                // the final batch is performing device I/O.
                state.staging = retry_staging;
                (batch, needs_checkpoint, retry_staging)
            };
            self.execute_writeback(batch)?;
            if needs_checkpoint {
                self.checkpoint_with_commit_gate()?;
            }
            if !retry_staging {
                return Ok(());
            }
        }
    }

    fn checkpoint_with_commit_gate(&self) -> Ext4Result<()> {
        let checkpoint = self.lock().ext4.prepare_writeback_checkpoint()?;
        self.execute_writeback(checkpoint)
    }

    fn execute_writeback(&self, batch: PreparedCommit<Ext4Disk>) -> Ext4Result<()> {
        let ticket = batch.ticket();
        let mut receipt = batch.execute();
        let mut result = self.lock().ext4.finish_sync(&mut receipt);
        if receipt.needs_abort_persistence() {
            receipt.persist_abort();
            result = self.lock().ext4.finish_sync(&mut receipt);
        }
        if let Err(error) = result {
            // Preserve both the original I/O cause and any failure recording
            // the abort superblock; do not reduce this to a generic VFS errno.
            log::error!(
                "ext4 writeback ticket {} failed: {error}; {receipt:?}",
                ticket.generation()
            );
        }
        result
    }

    /// Legacy indirect writes cannot release their rollback owner mid-call.
    /// Drain older detached work, then temporarily use the synchronous core
    /// while holding state exclusion. Extent writes never enter this path.
    pub(crate) fn write_legacy_inode(
        &self,
        inode: InodeNumber,
        offset: u64,
        bytes: &[u8],
    ) -> Ext4Result<()> {
        let _operation = self.admission.enter()?;
        let _commit = self.writeback.gate.lock();
        self.sync_with_commit_gate(CheckpointPolicy::Drain)?;
        let mut state = self.lock();
        if self.writeback.enabled {
            state.ext4.disable_background_writeback()?;
        }
        state.dirty = true;
        let result = state.ext4.write_inode(inode, offset, bytes);
        if self.writeback.enabled {
            // Re-enable without another mount-time sync. The caller retains
            // exclusion, so no mutation can observe the temporary mode.
            let restored = state.ext4.resume_background_writeback();
            if let Err(error) = restored {
                log::error!(
                    "ext4 legacy-write compatibility exit failed: {error}; original write result: \
                     {result:?}"
                );
                return result.and(Err(error));
            }
        }
        result
    }

    pub(super) fn start_writeback_worker(&self) -> VfsResult<()> {
        if self.writeback.enabled {
            self.writeback.task.start(
                "ext4-commit",
                self.self_ref.clone(),
                run_writeback_worker,
            )?;
            log::info!("ext4 background page and journal writeback enabled, interval 5 seconds");
        }
        Ok(())
    }

    /// Flush a finite set of cached files before sealing their metadata. Page
    /// writes can request journal progress, so they must run outside `gate`.
    pub(super) fn periodic_writeback(&self) -> VfsResult<()> {
        let pages = crate::file::writeback_filesystem_pages(self);

        // A failed page write can still have completed a valid prefix. Commit
        // that prefix as well, while returning the original page error first.
        let dirty = self.lock().dirty;
        let metadata = if dirty {
            self.sync_core(CheckpointPolicy::LogPressure)
                .inspect_err(|error| log::error!("ext4 periodic commit failed: {error}"))
                .map_err(into_vfs_err)
        } else {
            Ok(())
        };
        pages.and(metadata)
    }
}

fn run_writeback_worker(
    filesystem: Weak<Ext4Filesystem>,
    notification: Arc<dyn BlockNotification>,
    stopping: Arc<AtomicBool>,
) {
    let mut deadline = crate::os::monotonic_time().saturating_add(COMMIT_INTERVAL);
    while !stopping.load(Ordering::Acquire) {
        notification.wait_timeout(deadline.saturating_sub(crate::os::monotonic_time()));
        if stopping.load(Ordering::Acquire) {
            break;
        }
        let Some(filesystem) = filesystem.upgrade() else {
            break;
        };
        // Anchor the next deadline before I/O: redirty during a slow commit
        // must not incur another full interval after that commit completes.
        deadline = crate::os::monotonic_time().saturating_add(COMMIT_INTERVAL);
        if let Err(error) = filesystem.periodic_writeback() {
            log::error!("ext4 periodic writeback failed: {error:?}");
        }
    }
}
