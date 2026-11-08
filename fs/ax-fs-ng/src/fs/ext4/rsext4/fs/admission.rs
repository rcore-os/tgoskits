//! Separate temporary reconfiguration pauses from terminal operation closure.

use rsext4::{Ext4Error, Ext4Result};

use crate::os::{sync::RawSpinLock, waiters::TaskWaiters};

/// Counts operations and owns the waiters for drain and resume transitions.
pub(super) struct Admission {
    /// One exclusion boundary for the phase, active owners, and first failure.
    state: RawSpinLock<OperationCount>,
    /// Notified after the final owner leaves or a temporary pause reopens.
    changed: TaskWaiters,
}

/// A paused gate waits for reconfiguration; a closed gate rejects new work.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AdmissionPhase {
    /// Admit new operations.
    Open,
    /// Wait for a reconfiguration owner to restore admission.
    Paused,
    /// Reject new operations while the shutdown owner drains existing work.
    Closing,
}

struct OperationCount {
    /// Whether a new caller enters, waits, or fails.
    phase: AdmissionPhase,
    /// Successful acquisitions not yet released by their guards.
    active: usize,
    /// A persistent cause that reopening must never clear.
    failure: Option<Ext4Error>,
}

/// Releases one successful operation acquisition on every return path.
pub(super) struct OperationGuard<'a>(&'a Admission);

/// Reopens a temporary pause on every success or error return.
pub(super) struct AdmissionPause<'a>(&'a Admission);

impl Admission {
    pub(super) const fn new() -> Self {
        Self {
            state: RawSpinLock::new(OperationCount {
                phase: AdmissionPhase::Open,
                active: 0,
                failure: None,
            }),
            changed: TaskWaiters::new(),
        }
    }

    pub(super) fn enter(&self) -> Ext4Result<OperationGuard<'_>> {
        self.acquire_operation()?;
        Ok(OperationGuard(self))
    }

    fn acquire_operation(&self) -> Ext4Result<()> {
        self.acquire_operation_with(|| {
            self.wait_while(|| self.state.lock_irqsave().phase == AdmissionPhase::Paused)
        })
    }

    fn acquire_operation_with(&self, mut wait: impl FnMut() -> Ext4Result<()>) -> Ext4Result<()> {
        loop {
            let mut state = self.state.lock_irqsave();
            if let Some(failure) = state.failure {
                return Err(failure);
            }
            match state.phase {
                AdmissionPhase::Open => {
                    state.active = state
                        .active
                        .checked_add(1)
                        .ok_or_else(Ext4Error::overflow)?;
                    return Ok(());
                }
                AdmissionPhase::Closing => {
                    return Err(Ext4Error::busy().with_operation("filesystem:closing"));
                }
                AdmissionPhase::Paused => drop(state),
            }
            // TaskWaiters registers before rechecking the predicate, so a
            // concurrent resume cannot be lost between inspection and sleep.
            wait()?;
        }
    }

    /// Drains existing operations while new callers wait for the pause owner.
    /// The lifecycle owner must serialize pauses against terminal closure.
    pub(super) fn pause_and_drain(&self) -> Ext4Result<AdmissionPause<'_>> {
        self.drain_with(AdmissionPhase::Paused, || {
            self.wait_while(|| self.state.lock_irqsave().active != 0)
        })?;
        Ok(AdmissionPause(self))
    }

    pub(super) fn close_and_drain(&self) -> Ext4Result<()> {
        self.close_and_drain_with(|| self.wait_while(|| self.state.lock_irqsave().active != 0))
    }

    fn wait_while(&self, should_wait: impl FnOnce() -> bool) -> Ext4Result<()> {
        self.changed.wait_while(should_wait).map_err(|error| {
            log::error!("ext4 operation wait failed: {error}");
            match error {
                crate::BlockError::NoMemory => Ext4Error::no_memory(),
                crate::BlockError::WouldBlock | crate::BlockError::ResourceBusy => {
                    Ext4Error::busy()
                }
                crate::BlockError::RuntimeUnavailable | crate::BlockError::Unsupported => {
                    Ext4Error::unsupported_capability("runtime:operation_wait")
                }
                crate::BlockError::TimedOut => Ext4Error::timeout(),
                _ => Ext4Error::io(),
            }
            .with_operation("filesystem:operation_wait")
        })
    }

    fn close_and_drain_with(&self, wait: impl FnMut() -> Ext4Result<()>) -> Ext4Result<()> {
        self.drain_with(AdmissionPhase::Closing, wait)
    }

    fn drain_with(
        &self,
        phase: AdmissionPhase,
        mut wait: impl FnMut() -> Ext4Result<()>,
    ) -> Ext4Result<()> {
        {
            let mut state = self.state.lock_irqsave();
            if state.phase != AdmissionPhase::Open {
                return Err(Ext4Error::busy().with_operation("filesystem:already_closing"));
            }
            state.phase = phase;
        }
        while self.state.lock_irqsave().active != 0 {
            if let Err(error) = wait() {
                self.reopen();
                return Err(error);
            }
        }
        Ok(())
    }

    pub(super) fn reopen(&self) {
        self.state.lock_irqsave().phase = AdmissionPhase::Open;
        // Publish before notifying, and never invoke runtime wakeups under
        // admission exclusion. A persistent failure remains authoritative.
        self.changed.notify_all();
    }

    #[cfg(test)]
    pub(super) fn active_count(&self) -> usize {
        self.state.lock_irqsave().active
    }

    /// Permanently rejects new cached mutations with the first journal cause.
    /// Reopening a failed shutdown must not clear an aborted journal's error.
    pub(super) fn fail(&self, cause: Ext4Error) {
        self.state.lock_irqsave().failure.get_or_insert(cause);
    }

    fn release_operation(&self) {
        let last = {
            let mut state = self.state.lock_irqsave();
            assert!(state.active != 0, "unbalanced filesystem operation release");
            state.active -= 1;
            state.active == 0
        };
        if last {
            self.changed.notify_all();
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

impl Drop for AdmissionPause<'_> {
    fn drop(&mut self) {
        self.0.reopen();
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
        for phase in [AdmissionPhase::Closing, AdmissionPhase::Paused] {
            let admission = Admission::new();
            let active = admission.enter().unwrap();
            let cause = Ext4Error::no_memory().with_operation("test:drain_wait");
            assert_eq!(admission.drain_with(phase, || Err(cause)), Err(cause));
            assert!(admission.enter().is_ok());
            drop(active);
            admission.close_and_drain().unwrap();
            assert!(admission.enter().is_err());
        }
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

    #[test]
    fn temporary_pause_waits_for_resume_but_terminal_closure_rejects() {
        let admission = Admission::new();
        let mut pause = Some(admission.pause_and_drain().unwrap());
        let mut waited = false;
        admission
            .acquire_operation_with(|| {
                assert!(!waited, "resume must make progress on the next attempt");
                waited = true;
                drop(pause.take());
                Ok(())
            })
            .unwrap();
        assert!(waited, "a paused operation entered before resume");
        admission.release_operation();
        admission.close_and_drain().unwrap();
        assert_eq!(
            admission.acquire_operation_with(|| panic!("terminal closure must not wait")),
            Err(Ext4Error::busy().with_operation("filesystem:closing"))
        );
    }
}
