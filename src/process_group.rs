use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::fmt;

use kspin::SpinNoIrq;

use crate::{Pgid, Pid, Process, Session};

/// A [`ProcessGroup`] is a collection of [`Process`]es.
pub struct ProcessGroup {
    pgid: Pgid,
    pub(crate) session: Arc<Session>,
    pub(crate) processes: SpinNoIrq<BTreeMap<Pid, Weak<Process>>>,
}

impl ProcessGroup {
    /// Create a new [`ProcessGroup`] within a [`Session`].
    pub(crate) fn new(pgid: Pgid, session: &Arc<Session>) -> Arc<Self> {
        let group = Arc::new(Self {
            pgid,
            session: session.clone(),
            processes: SpinNoIrq::new(BTreeMap::new()),
        });
        session
            .process_groups
            .lock()
            .insert(pgid, Arc::downgrade(&group));
        group
    }
}

impl ProcessGroup {
    /// The [`ProcessGroup`] ID.
    pub fn pgid(&self) -> Pgid {
        self.pgid
    }

    /// The [`Session`] that the [`ProcessGroup`] belongs to.
    pub fn session(&self) -> Arc<Session> {
        self.session.clone()
    }

    /// The [`Process`]es that belong to this [`ProcessGroup`].
    pub fn processes(&self) -> Vec<Arc<Process>> {
        self.processes
            .lock()
            .values()
            .filter_map(Weak::upgrade)
            .collect()
    }
}

impl fmt::Debug for ProcessGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessGroup")
            .field("pgid", &self.pgid)
            .field("session", &self.session)
            .finish()
    }
}
