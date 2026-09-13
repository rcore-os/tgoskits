//! Drain admitted operations without holding filesystem or commit exclusion.

use rsext4::{Ext4Error, Ext4Result};

use crate::os::{sync::IrqMutex, waiters::TaskWaiters};

pub(super) struct Admission {
    state: IrqMutex<OperationCount>,
    drained: TaskWaiters,
}

struct OperationCount {
    closing: bool,
    active: usize,
    failure: Option<Ext4Error>,
}

pub(super) struct OperationGuard<'a>(&'a Admission);

impl Admission {
    pub(super) const fn new() -> Self {
        Self {
            state: IrqMutex::new(OperationCount {
                closing: false,
                active: 0,
                failure: None,
            }),
            drained: TaskWaiters::new(),
        }
    }

    pub(super) fn enter(&self) -> Ext4Result<OperationGuard<'_>> {
        self.acquire_operation()?;
        Ok(OperationGuard(self))
    }

    fn acquire_operation(&self) -> Ext4Result<()> {
        let mut state = self.state.lock();
        if let Some(failure) = state.failure {
            return Err(failure);
        }
        if state.closing {
            return Err(Ext4Error::busy().with_operation("filesystem:closing"));
        }
        state.active = state
            .active
            .checked_add(1)
            .ok_or_else(Ext4Error::overflow)?;
        Ok(())
    }

    pub(super) fn close_and_drain(&self) -> Ext4Result<()> {
        self.close_and_drain_with(|| {
            self.drained
                .wait_while(|| self.state.lock().active != 0)
                .map_err(|error| {
                    log::error!("ext4 operation drain failed: {error}");
                    match error {
                        crate::BlockError::NoMemory => Ext4Error::no_memory(),
                        crate::BlockError::WouldBlock | crate::BlockError::ResourceBusy => {
                            Ext4Error::busy()
                        }
                        crate::BlockError::RuntimeUnavailable | crate::BlockError::Unsupported => {
                            Ext4Error::unsupported_capability("runtime:operation_drain")
                        }
                        crate::BlockError::TimedOut => Ext4Error::timeout(),
                        _ => Ext4Error::io(),
                    }
                    .with_operation("filesystem:operation_drain")
                })
        })
    }

    fn close_and_drain_with(&self, mut wait: impl FnMut() -> Ext4Result<()>) -> Ext4Result<()> {
        {
            let mut state = self.state.lock();
            if state.closing {
                return Err(Ext4Error::busy().with_operation("filesystem:already_closing"));
            }
            state.closing = true;
        }
        while self.state.lock().active != 0 {
            if let Err(error) = wait() {
                self.reopen();
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) fn reopen(&self) {
        self.state.lock().closing = false;
    }

    /// Permanently rejects new cached mutations with the first journal cause.
    /// Reopening a failed shutdown must not clear an aborted journal's error.
    pub(super) fn fail(&self, cause: Ext4Error) {
        self.state.lock().failure.get_or_insert(cause);
    }

    fn release_operation(&self) {
        let last = {
            let mut state = self.state.lock();
            assert!(state.active != 0, "unbalanced filesystem operation release");
            state.active -= 1;
            state.active == 0
        };
        if last {
            self.drained.notify_all();
        }
    }
}

impl axfs_ng_vfs::CachedWriteAdmission for Admission {
    fn acquire(&self) -> axfs_ng_vfs::VfsResult<()> {
        self.acquire_operation().map_err(super::into_vfs_err)
    }

    fn release(&self) {
        self.release_operation();
    }
}

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        self.0.release_operation();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_drain_reopens_admission_and_preserves_the_wait_error() {
        let admission = Admission::new();
        let active = admission.enter().unwrap();
        let cause = Ext4Error::no_memory().with_operation("test:drain_wait");
        assert_eq!(admission.close_and_drain_with(|| Err(cause)), Err(cause));
        assert!(admission.enter().is_ok());
        drop(active);
        admission.close_and_drain().unwrap();
        assert!(admission.enter().is_err());
    }

    #[test]
    fn reopening_after_abort_preserves_the_first_failure() {
        let admission = Admission::new();
        let active = admission.enter().unwrap();
        let cause = Ext4Error::io().with_operation("test:journal_write");
        admission.fail(cause);
        admission.fail(Ext4Error::timeout());
        assert_eq!(admission.enter().err(), Some(cause));
        drop(active);
        admission.close_and_drain().unwrap();
        admission.reopen();
        assert_eq!(admission.enter().err(), Some(cause));
    }
}
