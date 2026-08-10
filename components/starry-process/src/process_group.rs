use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::fmt;

use ax_kspin::SpinNoIrq;
use weak_map::WeakMap;

use crate::{Pid, Process, Session};

/// A [`ProcessGroup`] is a collection of [`Process`]es.
pub struct ProcessGroup {
    pgid: Pid,
    pub(crate) session: Arc<Session>,
    pub(crate) processes: SpinNoIrq<WeakMap<Pid, Weak<Process>>>,
}

impl ProcessGroup {
    /// Returns the canonical live process group for `pgid` in `session`.
    ///
    /// The session registry serializes process-group creation so that racing
    /// parent and child `setpgid()` calls converge on one group identity.
    pub(crate) fn get_or_create(pgid: Pid, session: &Arc<Session>) -> Arc<Self> {
        let group = Arc::new(Self {
            pgid,
            session: session.clone(),
            processes: SpinNoIrq::new(WeakMap::new()),
        });

        let mut groups = session.process_groups.lock();
        if let Some(existing) = groups.get(&pgid) {
            existing
        } else {
            groups.insert(pgid, &group);
            group
        }
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
        self.processes.lock().values().collect()
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
    extern crate std;

    use std::{sync::Barrier, thread};

    use super::*;

    #[test]
    fn duplicate_live_group_identity_reuses_the_session_group() {
        let session = Session::new(7);
        let start = Arc::new(Barrier::new(2));

        let first_session = session.clone();
        let first_start = start.clone();
        let first = thread::spawn(move || {
            first_start.wait();
            ProcessGroup::get_or_create(11, &first_session)
        });
        let second = thread::spawn(move || {
            start.wait();
            ProcessGroup::get_or_create(11, &session)
        });

        let first = first.join().unwrap();
        let second = second.join().unwrap();
        let session = first.session();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(session.process_groups().len(), 1);
    }
}
