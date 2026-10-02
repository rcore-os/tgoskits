//! Sleepable reader/writer admission for inode and namespace ownership, not data storage.

use alloc::sync::Arc;

use axfs_ng_vfs::{VfsError, VfsResult};

use crate::{
    error::block_error_to_vfs_error,
    os::{sync::IrqMutex, waiters::TaskWaiters},
};

pub(crate) struct AccessGate {
    state: IrqMutex<AccessState>,
    changed: TaskWaiters,
}

struct AccessState {
    readers: usize,
    waiting_writers: usize,
    writer: bool,
}

#[must_use = "retain shared access until all protected reads complete"]
pub(crate) struct ReadAccess(Arc<AccessGate>);
#[must_use = "retain exclusive access until the protected mutation completes"]
pub(crate) struct WriteAccess(Arc<AccessGate>);

struct WaitingWriter {
    gate: Arc<AccessGate>,
    queued: bool,
}

impl AccessGate {
    pub(crate) fn new() -> Self {
        Self {
            state: IrqMutex::new(AccessState {
                readers: 0,
                waiting_writers: 0,
                writer: false,
            }),
            changed: TaskWaiters::new(),
        }
    }

    pub(crate) fn read(self: &Arc<Self>) -> VfsResult<ReadAccess> {
        if let Some(access) = self.try_read()? {
            return Ok(access);
        }
        self.read_with(|| {
            self.changed
                .wait_while(|| {
                    let state = self.state.lock();
                    state.writer || state.waiting_writers != 0
                })
                .map_err(block_error_to_vfs_error)
        })
    }

    fn read_with(
        self: &Arc<Self>,
        mut wait: impl FnMut() -> VfsResult<()>,
    ) -> VfsResult<ReadAccess> {
        loop {
            if let Some(access) = self.try_read()? {
                return Ok(access);
            }
            wait()?;
        }
    }

    /// Readers share stable mappings but never bypass an already queued writer.
    pub(crate) fn try_read(self: &Arc<Self>) -> VfsResult<Option<ReadAccess>> {
        let mut state = self.state.lock();
        if state.writer || state.waiting_writers != 0 {
            return Ok(None);
        }
        state.readers = state
            .readers
            .checked_add(1)
            .ok_or(VfsError::ValueOverflow)?;
        Ok(Some(ReadAccess(self.clone())))
    }

    pub(crate) fn write(self: &Arc<Self>) -> VfsResult<WriteAccess> {
        if let Some(access) = self.try_write() {
            return Ok(access);
        }
        self.write_with(|| {
            self.changed
                .wait_while(|| {
                    let state = self.state.lock();
                    state.writer || state.readers != 0
                })
                .map_err(block_error_to_vfs_error)
        })
    }

    /// Claims idle access without overtaking a writer already waiting for it.
    pub(crate) fn try_write(self: &Arc<Self>) -> Option<WriteAccess> {
        let mut state = self.state.lock();
        if state.writer || state.readers != 0 || state.waiting_writers != 0 {
            return None;
        }
        state.writer = true;
        Some(WriteAccess(self.clone()))
    }

    fn write_with(
        self: &Arc<Self>,
        mut wait: impl FnMut() -> VfsResult<()>,
    ) -> VfsResult<WriteAccess> {
        {
            let mut state = self.state.lock();
            state.waiting_writers = state
                .waiting_writers
                .checked_add(1)
                .ok_or(VfsError::ValueOverflow)?;
        }
        let mut waiting = WaitingWriter {
            gate: self.clone(),
            queued: true,
        };
        loop {
            {
                let mut state = self.state.lock();
                if !state.writer && state.readers == 0 {
                    state.waiting_writers -= 1;
                    state.writer = true;
                    waiting.queued = false;
                    return Ok(WriteAccess(self.clone()));
                }
            }
            wait()?;
        }
    }
}

impl Drop for ReadAccess {
    fn drop(&mut self) {
        let last = {
            let mut state = self.0.state.lock();
            assert!(state.readers != 0, "unbalanced access read release");
            state.readers -= 1;
            state.readers == 0
        };
        if last {
            self.0.changed.notify_all();
        }
    }
}

impl Drop for WriteAccess {
    fn drop(&mut self) {
        {
            let mut state = self.0.state.lock();
            assert!(state.writer, "unbalanced access write release");
            state.writer = false;
        }
        self.0.changed.notify_all();
    }
}

impl Drop for WaitingWriter {
    fn drop(&mut self) {
        if self.queued {
            {
                let mut state = self.gate.state.lock();
                assert!(
                    state.waiting_writers != 0,
                    "unbalanced access writer cancellation"
                );
                state.waiting_writers -= 1;
            }
            self.gate.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests;
