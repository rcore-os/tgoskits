use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
};

use axsync::{Mutex, MutexGuard};

use crate::{Pgid, Pid, Process, Session, process_group_table};

/// A [`ProcessGroup`] is a collection of [`Process`]es.
pub struct ProcessGroup {
    pgid: Pgid,
    inner: Mutex<ProcessGroupInner>,
}

pub(crate) struct ProcessGroupInner {
    pub(crate) processes: BTreeMap<Pid, Arc<Process>>,
    pub(crate) session: Weak<Session>,
}

impl ProcessGroup {
    /// Create a new [`ProcessGroup`] from a [`Process`].
    pub(crate) fn new(process: &Arc<Process>) -> Arc<Self> {
        let pgid = process.pid();

        let mut processes = BTreeMap::new();
        processes.insert(pgid, process.clone());

        let group = Arc::new(Self {
            pgid,
            inner: Mutex::new(ProcessGroupInner {
                processes,
                session: Weak::new(),
            }),
        });
        process_group_table().insert(pgid, group.clone());

        process.inner().group = Arc::downgrade(&group);
        group
    }

    pub(crate) fn inner(&self) -> MutexGuard<ProcessGroupInner> {
        self.inner.lock()
    }

    /// The [`ProcessGroup`] ID.
    pub fn pgid(&self) -> Pgid {
        self.pgid
    }

    /// The [`Session`] that the [`ProcessGroup`] belongs to.
    pub fn session(&self) -> Arc<Session> {
        // See the comments in `Process::group` for this `unwrap`.
        self.inner().session.upgrade().unwrap()
    }
}
