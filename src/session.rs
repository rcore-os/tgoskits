use alloc::{collections::btree_map::BTreeMap, sync::Arc};

use kspin::{SpinNoIrq, SpinNoIrqGuard};

use crate::{Pgid, ProcessGroup, Sid, session_table};

/// A [`Session`] is a collection of [`ProcessGroup`]s.
pub struct Session {
    sid: Sid,
    inner: SpinNoIrq<SessionInner>,
}

pub(crate) struct SessionInner {
    pub(crate) process_groups: BTreeMap<Pgid, Arc<ProcessGroup>>,
    // TODO: shell job control
}

impl Session {
    /// Create a new [`Session`] from a [`ProcessGroup`].
    pub(crate) fn new(group: &Arc<ProcessGroup>) -> Arc<Self> {
        let sid = group.pgid();

        let mut process_groups = BTreeMap::new();
        process_groups.insert(sid, group.clone());

        let session = Arc::new(Self {
            sid,
            inner: SpinNoIrq::new(SessionInner { process_groups }),
        });
        session_table().insert(sid, session.clone());

        group.inner().session = Arc::downgrade(&session);
        session
    }

    pub(crate) fn inner(&self) -> SpinNoIrqGuard<SessionInner> {
        self.inner.lock()
    }

    /// The [`Session`] ID.
    pub fn sid(&self) -> Sid {
        self.sid
    }
}
