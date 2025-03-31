use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};

use kspin::{SpinNoIrq, SpinNoIrqGuard};

use crate::{Pgid, Pid, Process, Session};

/// A [`ProcessGroup`] is a collection of [`Process`]es.
pub struct ProcessGroup {
    pgid: Pgid,
    inner: SpinNoIrq<ProcessGroupInner>,
}

pub(crate) struct ProcessGroupInner {
    pub(crate) processes: BTreeMap<Pid, Weak<Process>>,
    pub(crate) session: Arc<Session>,
}

impl ProcessGroupInner {
    pub(crate) fn processes(&self) -> impl DoubleEndedIterator<Item = Arc<Process>> {
        self.processes.values().filter_map(Weak::upgrade)
    }
}

impl ProcessGroup {
    /// Create a new [`ProcessGroup`] within a [`Session`].
    pub(crate) fn new(pgid: Pgid, session: &Arc<Session>) -> Arc<Self> {
        let group = Arc::new(Self {
            pgid,
            inner: SpinNoIrq::new(ProcessGroupInner {
                processes: BTreeMap::new(),
                session: session.clone(),
            }),
        });
        session
            .inner()
            .process_groups
            .insert(pgid, Arc::downgrade(&group));
        group
    }

    pub(crate) fn inner(&self) -> SpinNoIrqGuard<ProcessGroupInner> {
        self.inner.lock()
    }
}

impl ProcessGroup {
    /// The [`ProcessGroup`] ID.
    pub fn pgid(&self) -> Pgid {
        self.pgid
    }

    /// The [`Session`] that the [`ProcessGroup`] belongs to.
    pub fn session(&self) -> Arc<Session> {
        self.inner().session.clone()
    }

    /// The [`Process`]es that belong to this [`ProcessGroup`].
    pub fn processes(&self) -> Vec<Arc<Process>> {
        self.inner().processes().collect()
    }
}
