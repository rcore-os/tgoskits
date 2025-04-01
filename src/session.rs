use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::fmt;

use kspin::SpinNoIrq;

use crate::{Pgid, ProcessGroup, Sid};

/// A [`Session`] is a collection of [`ProcessGroup`]s.
pub struct Session {
    sid: Sid,
    pub(crate) process_groups: SpinNoIrq<BTreeMap<Pgid, Weak<ProcessGroup>>>,
    // TODO: shell job control
}

impl Session {
    /// Create a new [`Session`].
    pub(crate) fn new(sid: Sid) -> Arc<Self> {
        Arc::new(Self {
            sid,
            process_groups: SpinNoIrq::new(BTreeMap::new()),
        })
    }
}

impl Session {
    /// The [`Session`] ID.
    pub fn sid(&self) -> Sid {
        self.sid
    }

    /// The [`ProcessGroup`]s that belong to this [`Session`].
    pub fn process_groups(&self) -> Vec<Arc<ProcessGroup>> {
        self.process_groups
            .lock()
            .values()
            .filter_map(Weak::upgrade)
            .collect()
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").field("sid", &self.sid).finish()
    }
}
