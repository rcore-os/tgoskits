//! One mutation attempt and its lock-external journal pressure/abort completion.

use rsext4::Ext4Result;

use super::{Ext4Filesystem, Ext4State, writeback::CheckpointPolicy};

/// One mount-serialized attempt, before any lock-external recovery or wait.
pub(super) enum MutationAttempt<T> {
    Finished(Ext4Result<T>),
    Staging,
}

impl Ext4Filesystem {
    /// Retries only the core's typed, restartable journal-space boundary.
    /// The caller must hold inode content exclusion for multi-step operations
    /// and retain explicit continuation state where size publication is partial.
    pub(crate) fn with_writeback_progress<T>(
        &self,
        operation: impl FnMut(&mut Ext4State) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        let _operation = self.admission.enter()?;
        self.with_admitted_writeback_progress(operation)
    }

    /// The caller retains an admission guard across a lock-external phase.
    pub(super) fn with_admitted_writeback_progress<T>(
        &self,
        mut operation: impl FnMut(&mut Ext4State) -> Ext4Result<T>,
    ) -> Ext4Result<T> {
        loop {
            let attempt = self.attempt_admitted_mutation(&mut operation);
            if let Some(value) = self.finish_mutation_attempt(attempt)? {
                return Ok(value);
            }
        }
    }

    /// The caller owns admission and the operation's domain exclusion. Nothing
    /// in this phase waits for the separate commit owner after releasing state.
    pub(super) fn attempt_admitted_mutation<T>(
        &self,
        operation: impl FnOnce(&mut Ext4State) -> Ext4Result<T>,
    ) -> MutationAttempt<T> {
        let mut state = self.lock();
        // Even an error can leave a completed prefix (e.g. unwritten
        // reservations). Schedule that valid metadata for writeback.
        state.dirty = true;
        if state.staging {
            MutationAttempt::Staging
        } else {
            MutationAttempt::Finished(operation(&mut state))
        }
    }

    /// Complete an attempt outside mount state. Atomic namespace callers also
    /// release namespace rights here; partial file mutations retain their
    /// content owner across this same pressure/abort protocol.
    pub(super) fn finish_mutation_attempt<T>(
        &self,
        attempt: MutationAttempt<T>,
    ) -> Ext4Result<Option<T>> {
        match attempt {
            MutationAttempt::Staging => self.sync_core(CheckpointPolicy::LogPressure)?,
            MutationAttempt::Finished(Err(error)) if error.requires_journal_progress() => {
                self.sync_core(CheckpointPolicy::LogPressure)?;
            }
            MutationAttempt::Finished(Err(error)) => {
                self.persist_latched_abort();
                return Err(error);
            }
            MutationAttempt::Finished(Ok(value)) => return Ok(Some(value)),
        }
        Ok(None)
    }
}
