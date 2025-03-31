use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};

use kspin::{SpinNoIrq, SpinNoIrqGuard};

use crate::{Pgid, ProcessGroup, Sid};

/// A [`Session`] is a collection of [`ProcessGroup`]s.
pub struct Session {
    sid: Sid,
    inner: SpinNoIrq<SessionInner>,
}

pub(crate) struct SessionInner {
    pub(crate) process_groups: BTreeMap<Pgid, Weak<ProcessGroup>>,
    // TODO: shell job control
}

impl SessionInner {
    pub(crate) fn process_groups(&self) -> impl DoubleEndedIterator<Item = Arc<ProcessGroup>> {
        self.process_groups.values().filter_map(Weak::upgrade)
    }
}

impl Session {
    /// Create a new [`Session`].
    pub(crate) fn new(sid: Sid) -> Arc<Self> {
        Arc::new(Self {
            sid,
            inner: SpinNoIrq::new(SessionInner {
                process_groups: BTreeMap::new(),
            }),
        })
    }

    pub(crate) fn inner(&self) -> SpinNoIrqGuard<SessionInner> {
        self.inner.lock()
    }
}

impl Session {
    /// The [`Session`] ID.
    pub fn sid(&self) -> Sid {
        self.sid
    }

    /// The [`ProcessGroup`]s that belong to this [`Session`].
    pub fn process_groups(&self) -> Vec<Arc<ProcessGroup>> {
        self.inner().process_groups().collect()
    }
}
