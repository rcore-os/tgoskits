//! Generation-checked thread park handshake.

use crate::{
    ScheduleDecision, ThreadId,
    timer::{TaskDeadlineRegistration, TaskDeadlineToken},
};

/// Move-only ownership of one park attempt and its optional timeout deadline.
///
/// ```compile_fail
/// # use ax_task::ParkTicket;
/// fn duplicate(ticket: ParkTicket) {
///     let first = ticket;
///     let second = ticket;
///     drop((first, second));
/// }
/// ```
#[must_use = "a prepared park and its deadline must be committed or cancelled"]
#[derive(Debug, Eq, PartialEq)]
pub struct ParkTicket {
    thread: ThreadId,
    generation: u64,
    deadline: Option<TaskDeadlineRegistration>,
    resolved: bool,
}

impl ParkTicket {
    pub(crate) const fn new(thread: ThreadId, generation: u64) -> Self {
        Self {
            thread,
            generation,
            deadline: None,
            resolved: false,
        }
    }

    /// Returns the thread that prepared this park attempt.
    pub const fn thread(&self) -> ThreadId {
        self.thread
    }

    /// Returns the monotonically increasing attempt generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn attach_deadline(
        &mut self,
        deadline: TaskDeadlineRegistration,
    ) -> Result<(), TaskDeadlineRegistration> {
        if self.deadline.is_some() {
            Err(deadline)
        } else {
            self.deadline = Some(deadline);
            Ok(())
        }
    }

    pub(crate) const fn deadline(&self) -> Option<&TaskDeadlineRegistration> {
        self.deadline.as_ref()
    }

    pub(crate) fn clear_deadline(&mut self, deadline: TaskDeadlineToken) -> bool {
        if self
            .deadline
            .as_ref()
            .is_some_and(|registration| registration.token() == deadline)
        {
            let _registration = self.deadline.take();
            true
        } else {
            false
        }
    }

    pub(crate) fn mark_resolved(&mut self) {
        self.resolved = true;
    }

    pub(crate) const fn is_resolved(&self) -> bool {
        self.resolved
    }

    pub(crate) const fn has_deadline(&self) -> bool {
        self.deadline.is_some()
    }
}

/// Result of publishing the `PARKING` phase.
#[derive(Debug, Eq, PartialEq)]
pub enum ParkPrepare {
    /// A preceding notification was consumed, so the caller must not block.
    Notified,
    /// The caller published `PARKING` and may proceed to the commit phase.
    Prepared(ParkTicket),
}

/// Result of rechecking a prepared park at the scheduler safe point.
#[derive(Clone, Copy, Debug)]
pub enum ParkCommit {
    /// A concurrent notification cancelled the park before schedule-out.
    Notified,
    /// The thread committed `BLOCKED` and selected its replacement.
    Blocked(ScheduleDecision),
}
