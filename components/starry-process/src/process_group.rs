use alloc::{sync::Arc, vec::Vec};
use core::fmt;

use crate::{
    Pid, Process, Session,
    relations::{GroupMembers, ProcessRelationTxn, RelationLock},
};

/// A [`ProcessGroup`] is a collection of [`Process`]es.
pub struct ProcessGroup {
    pgid: Pid,
    pub(crate) session: Arc<Session>,
    pub(crate) processes: RelationLock<GroupMembers>,
}

impl ProcessGroup {
    /// Returns the canonical live process group for `pgid` in `session`.
    ///
    /// Linux serializes process-group creation with the task-list lock. The
    /// session registry is the corresponding identity authority here: racing
    /// parent/child `setpgid()` calls must converge on one group rather than
    /// creating two objects with the same PGID.
    pub(crate) fn get_or_create(pgid: Pid, session: &Arc<Session>) -> Arc<Self> {
        let group = Arc::new(Self {
            pgid,
            session: session.clone(),
            // The creating process can join without allocating while the
            // membership transaction is held.
            processes: RelationLock::new(GroupMembers::with_capacity(1)),
        });
        ProcessRelationTxn::attach_session_group(&group)
    }
}

impl ProcessGroup {
    /// The [`ProcessGroup`] ID.
    pub fn pgid(&self) -> Pid {
        self.pgid
    }

    /// The [`Session`] that the [`ProcessGroup`] belongs to.
    pub fn session(&self) -> Arc<Session> {
        self.session.clone()
    }

    /// The [`Process`]es that belong to this [`ProcessGroup`].
    pub fn processes(&self) -> Vec<Arc<Process>> {
        loop {
            let member_count = self.processes.lock().len();
            let mut processes = Vec::with_capacity(member_count);
            let members = self.processes.lock();
            if processes.capacity() < members.len() {
                drop(members);
                continue;
            }
            members.snapshot(&mut processes);
            return processes;
        }
    }
}

impl fmt::Debug for ProcessGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ProcessGroup({}, session={})",
            self.pgid,
            self.session.sid()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_live_group_identity_reuses_the_session_group() {
        let session = Session::new(7);
        let first = ProcessGroup::get_or_create(11, &session);
        let second = ProcessGroup::get_or_create(11, &session);

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(session.process_groups().len(), 1);
    }
}
