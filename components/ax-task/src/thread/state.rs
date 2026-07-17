//! Checked thread lifecycle transitions.

use crate::TaskError;

/// Observable lifecycle state of a thread.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadState {
    /// Allocated but not admitted to a run queue.
    New     = 0,
    /// Eligible to run or already present in a run queue.
    Ready   = 1,
    /// Currently executing on a CPU.
    Running = 2,
    /// Publishing a block operation while racing with wake-up.
    Parking = 3,
    /// Asleep on a wait object.
    Blocked = 4,
    /// A wake operation won the block/wake race.
    Waking  = 5,
    /// Execution has terminated and resources await reaping.
    Exited  = 6,
}

/// Checked lifecycle state owned by the task registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThreadLifecycle {
    state: ThreadState,
}

impl ThreadLifecycle {
    pub(crate) const fn new() -> Self {
        Self {
            state: ThreadState::New,
        }
    }

    pub(crate) const fn state(self) -> ThreadState {
        self.state
    }

    pub(crate) fn transition(&mut self, next: ThreadState) -> Result<(), TaskError> {
        if transition_is_valid(self.state, next) {
            self.state = next;
            Ok(())
        } else {
            Err(TaskError::InvalidTransition {
                from: self.state,
                to: next,
            })
        }
    }

    /// Rolls back a Deadline replenishment that could not publish its timer.
    ///
    /// This is deliberately narrower than a general reverse transition: only
    /// the temporary `Blocked/Waking/Ready -> Ready` preparation performed by
    /// `TaskSystem::replenish_deadline` may use it.
    pub(crate) fn rollback_deadline_replenishment(
        &mut self,
        previous: ThreadState,
    ) -> Result<(), TaskError> {
        if self.state == ThreadState::Ready
            && matches!(
                previous,
                ThreadState::Blocked | ThreadState::Waking | ThreadState::Ready
            )
        {
            self.state = previous;
            Ok(())
        } else {
            Err(TaskError::InvalidTransition {
                from: self.state,
                to: previous,
            })
        }
    }
}

const fn transition_is_valid(from: ThreadState, to: ThreadState) -> bool {
    matches!(
        (from, to),
        (ThreadState::New, ThreadState::Ready | ThreadState::Exited)
            | (
                ThreadState::Ready,
                ThreadState::Running | ThreadState::Exited
            )
            | (
                ThreadState::Running,
                ThreadState::Ready
                    | ThreadState::Parking
                    | ThreadState::Blocked
                    | ThreadState::Exited
            )
            | (
                ThreadState::Parking,
                ThreadState::Running
                    | ThreadState::Blocked
                    | ThreadState::Waking
                    | ThreadState::Ready
            )
            | (
                ThreadState::Blocked,
                ThreadState::Waking | ThreadState::Exited
            )
            | (
                ThreadState::Waking,
                ThreadState::Ready | ThreadState::Exited
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_documented_wake_transition() {
        let mut lifecycle = ThreadLifecycle::new();
        lifecycle.transition(ThreadState::Ready).unwrap();
        lifecycle.transition(ThreadState::Running).unwrap();
        lifecycle.transition(ThreadState::Parking).unwrap();
        lifecycle.transition(ThreadState::Waking).unwrap();
        lifecycle.transition(ThreadState::Ready).unwrap();
        assert_eq!(lifecycle.state(), ThreadState::Ready);
    }

    #[test]
    fn rejects_ready_to_blocked_shortcut() {
        let mut lifecycle = ThreadLifecycle::new();
        lifecycle.transition(ThreadState::Ready).unwrap();
        assert!(matches!(
            lifecycle.transition(ThreadState::Blocked),
            Err(TaskError::InvalidTransition { .. })
        ));
    }
}
