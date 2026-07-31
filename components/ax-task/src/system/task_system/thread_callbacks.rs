//! Typed deferred callback claims retained by one thread record.

use crate::TaskError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExitCallbackState {
    Absent,
    Pending,
    Claimed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeadlineCallbackState {
    Idle,
    Claimed,
}

/// Exit and Deadline callback ownership under the thread registry lock.
#[derive(Debug)]
pub(super) struct ThreadCallbackState {
    exit: ExitCallbackState,
    deadline: DeadlineCallbackState,
}

impl ThreadCallbackState {
    pub(super) const fn new() -> Self {
        Self {
            exit: ExitCallbackState::Absent,
            deadline: DeadlineCallbackState::Idle,
        }
    }

    pub(super) fn prepare_exit(&mut self, has_callback: bool) -> Result<(), TaskError> {
        if self.exit != ExitCallbackState::Absent {
            return Err(TaskError::InvalidConfiguration);
        }
        self.exit = if has_callback {
            ExitCallbackState::Pending
        } else {
            ExitCallbackState::Absent
        };
        Ok(())
    }

    pub(super) const fn exit_is_pending(&self) -> bool {
        matches!(self.exit, ExitCallbackState::Pending)
    }

    pub(super) fn claim_exit(&mut self) -> Result<(), TaskError> {
        if self.exit != ExitCallbackState::Pending {
            return Err(TaskError::InvalidConfiguration);
        }
        self.exit = ExitCallbackState::Claimed;
        Ok(())
    }

    pub(super) fn finish_exit(&mut self) -> Result<(), TaskError> {
        if self.exit != ExitCallbackState::Claimed {
            return Err(TaskError::InvalidConfiguration);
        }
        self.exit = ExitCallbackState::Absent;
        Ok(())
    }

    pub(super) fn blocks_reap(&self) -> bool {
        self.exit != ExitCallbackState::Absent || self.deadline != DeadlineCallbackState::Idle
    }

    pub(super) const fn deadline_is_claimed(&self) -> bool {
        matches!(self.deadline, DeadlineCallbackState::Claimed)
    }

    pub(super) fn claim_deadline(&mut self) -> Result<(), TaskError> {
        if self.deadline != DeadlineCallbackState::Idle {
            return Err(TaskError::InvalidConfiguration);
        }
        self.deadline = DeadlineCallbackState::Claimed;
        Ok(())
    }

    /// Completes exactly one claimed callback.
    pub(super) fn finish_deadline(&mut self) -> Result<(), TaskError> {
        if self.deadline != DeadlineCallbackState::Claimed {
            return Err(TaskError::InvalidConfiguration);
        }
        self.deadline = DeadlineCallbackState::Idle;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_claim_has_one_pending_to_claimed_to_absent_path() {
        let mut callbacks = ThreadCallbackState::new();
        callbacks.prepare_exit(true).unwrap();
        assert!(callbacks.exit_is_pending());
        assert_eq!(
            callbacks.prepare_exit(true),
            Err(TaskError::InvalidConfiguration)
        );
        callbacks.claim_exit().unwrap();
        assert!(callbacks.blocks_reap());
        assert_eq!(callbacks.claim_exit(), Err(TaskError::InvalidConfiguration));
        callbacks.finish_exit().unwrap();
        assert!(!callbacks.blocks_reap());
    }

    #[test]
    fn missing_exit_callback_never_blocks_reap() {
        let mut callbacks = ThreadCallbackState::new();
        callbacks.prepare_exit(false).unwrap();
        assert!(!callbacks.exit_is_pending());
        assert!(!callbacks.blocks_reap());
    }

    #[test]
    fn deadline_claim_blocks_reap_until_exactly_one_finish() {
        let mut callbacks = ThreadCallbackState::new();
        callbacks.claim_deadline().unwrap();
        assert!(callbacks.deadline_is_claimed());
        assert!(callbacks.blocks_reap());
        callbacks.finish_deadline().unwrap();
        assert!(!callbacks.blocks_reap());
        assert_eq!(
            callbacks.finish_deadline(),
            Err(TaskError::InvalidConfiguration)
        );
    }

    #[test]
    fn deadline_claim_has_one_idle_to_claimed_to_idle_path() {
        let mut callbacks = ThreadCallbackState::new();
        callbacks.claim_deadline().unwrap();
        assert_eq!(
            callbacks.claim_deadline(),
            Err(TaskError::InvalidConfiguration)
        );
        callbacks.finish_deadline().unwrap();
        assert!(!callbacks.blocks_reap());
    }
}
