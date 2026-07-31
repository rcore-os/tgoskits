use super::*;

/// Priority-inheritance graph result and Deadline CBS lending state.
#[derive(Debug)]
pub(in crate::system) struct ThreadPiState {
    pub(in crate::system) blocked_waiters: usize,
    pub(in crate::system) donor: Option<ThreadId>,
    pub(in crate::system) deadline_donor: Option<ThreadId>,
    pub(in crate::system) deadline_donor_core: Option<Weak<ThreadCore>>,
    pub(in crate::system) deadline_cbs_borrower: Option<ThreadId>,
    pub(in crate::system) deadline_cbs_generation: u64,
    pub(in crate::system) critical_rescue: bool,
}

impl ThreadPiState {
    pub(super) const fn new() -> Self {
        Self {
            blocked_waiters: 0,
            donor: None,
            deadline_donor: None,
            deadline_donor_core: None,
            deadline_cbs_borrower: None,
            deadline_cbs_generation: 1,
            critical_rescue: false,
        }
    }
}
