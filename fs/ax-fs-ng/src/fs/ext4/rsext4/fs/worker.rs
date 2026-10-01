//! Mount task ownership. Notification callbacks and joins run without locks.

use alloc::{boxed::Box, string::String, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::{
    BlockError, block_error_to_vfs_error,
    os::{BlockNotification, BlockThread, runtime_ops, sync::RawSpinLock},
};

type WorkerEntry = fn(Weak<Ext4Filesystem>, Arc<dyn BlockNotification>, Arc<AtomicBool>);

pub(super) struct MountWorker {
    stopping: Arc<AtomicBool>,
    notification: RawSpinLock<Option<Arc<dyn BlockNotification>>>,
    thread: RawSpinLock<Option<Box<dyn BlockThread>>>,
}

impl MountWorker {
    pub(super) fn disabled() -> Self {
        Self {
            stopping: Arc::new(AtomicBool::new(false)),
            notification: RawSpinLock::new(None),
            thread: RawSpinLock::new(None),
        }
    }

    /// Only mount construction or the exclusive shutdown owner starts tasks.
    /// A restart is valid after the previous join completes.
    pub(super) fn start(
        &self,
        name: &'static str,
        filesystem: Weak<Ext4Filesystem>,
        entry: WorkerEntry,
    ) -> VfsResult<()> {
        if self.thread.lock_irqsave().is_some() {
            return Ok(());
        }
        let runtime = runtime_ops().map_err(block_error_to_vfs_error)?;
        if !runtime.can_block() {
            return Err(block_error_to_vfs_error(BlockError::WouldBlock));
        }
        if filesystem.upgrade().is_none() {
            return Err(VfsError::BadState);
        }
        let notification = runtime.notification();
        let worker_notification = Arc::clone(&notification);
        let stopping = Arc::clone(&self.stopping);
        stopping.store(false, Ordering::Release);
        let thread = runtime
            .spawn_pinned(
                String::from(name),
                runtime.current_cpu(),
                Box::new(move || entry(filesystem, worker_notification, stopping)),
            )
            .map_err(block_error_to_vfs_error)?;
        *self.notification.lock_irqsave() = Some(notification);
        *self.thread.lock_irqsave() = Some(thread);
        Ok(())
    }

    pub(super) fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        let notification = self.notification.lock_irqsave().clone();
        if let Some(notification) = notification {
            notification.notify();
        }
    }

    pub(super) fn stop_and_join(&self) {
        self.stop();
        let thread = self.thread.lock_irqsave().take();
        if let Some(thread) = thread {
            thread.join();
        }
    }
}
