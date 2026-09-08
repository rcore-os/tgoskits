use super::*;
use crate::{PiWaitRegistration, PiWaitTree};

#[derive(Debug)]
pub(in crate::system) struct PiScheduleUpdate {
    pub(in crate::system) policy: SchedulePolicy,
    pub(in crate::system) donor: Option<ThreadId>,
    pub(in crate::system) deadline_donor: Option<ThreadId>,
    pub(in crate::system) deadline_donor_core: Option<Weak<ThreadCore>>,
    pub(in crate::system) deadline_donor_server: Option<DeadlineServer>,
    pub(in crate::system) generation: u64,
}

/// Priority-inheritance graph result and effective class state.
#[derive(Debug)]
pub(in crate::system) struct ThreadPiState {
    /// Mutex this task currently blocks on, protected by this task's PI lock.
    pub(in crate::system) blocked_on: Option<PiWaitRegistration>,
    /// Top waiter of every contended mutex owned by this task.
    ///
    /// This is Linux `task_struct::pi_waiters`: each physical mutex contributes
    /// at most its highest-priority waiter and the task lock owns every update.
    pub(in crate::system) donors: PiWaitTree,
    pub(in crate::system) donor: Option<ThreadId>,
    pub(in crate::system) deadline_donor: Option<ThreadId>,
    pub(in crate::system) deadline_donor_core: Option<Weak<ThreadCore>>,
}

impl ThreadPiState {
    pub(super) const fn new() -> Self {
        Self {
            blocked_on: None,
            donors: PiWaitTree::new(),
            donor: None,
            deadline_donor: None,
            deadline_donor_core: None,
        }
    }
}
