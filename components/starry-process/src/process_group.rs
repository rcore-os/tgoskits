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
    /// Create a new [`ProcessGroup`] within a [`Session`].
    pub(crate) fn new(pgid: Pid, session: &Arc<Session>) -> Arc<Self> {
        let group = Arc::new(Self {
            pgid,
            session: session.clone(),
            // The creating process can join without allocating while the
            // membership transaction is held.
            processes: RelationLock::new(GroupMembers::with_capacity(1)),
        });
        ProcessRelationTxn::attach_session_group(&group);
        group
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
