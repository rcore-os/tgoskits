use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::fmt;

use weak_map::WeakMap;

use super::{Process, Session};
use crate::{
    StarryResult,
    sync::SpinLock,
    task::{Pgid, PgidNumber, PidIdentity, PidRoleLease, TgidNumber},
};

/// A [`ProcessGroup`] is a collection of [`Process`]es.
pub struct ProcessGroup {
    pgid: PgidNumber,
    identity: Weak<PidIdentity>,
    _role: PidRoleLease<Pgid>,
    pub(crate) session: Arc<Session>,
    pub(crate) processes: SpinLock<WeakMap<TgidNumber, Weak<Process>>>,
}

impl ProcessGroup {
    /// Returns the canonical live process group for `pgid` in `session`.
    ///
    /// The session registry serializes process-group creation so that racing
    /// parent and child `setpgid()` calls converge on one group identity.
    pub(crate) fn get_or_create(
        identity: Arc<PidIdentity>,
        session: &Arc<Session>,
    ) -> StarryResult<Arc<Self>> {
        let pgid = PgidNumber::from(identity.root_number());
        let mut groups = session.process_groups.lock_irqsave();
        if let Some(existing) = groups.get(&pgid) {
            return Ok(existing);
        }
        let role = identity.acquire_role::<Pgid>()?;
        let group = Arc::new(Self {
            pgid,
            identity: Arc::downgrade(&identity),
            _role: role,
            session: session.clone(),
            processes: SpinLock::new(WeakMap::new()),
        });
        identity.bind_process_group(&group);
        groups.insert(pgid, &group);
        Ok(group)
    }
}

impl ProcessGroup {
    /// The root-namespace process-group ID.
    pub const fn pgid(&self) -> PgidNumber {
        self.pgid
    }

    pub(crate) const fn pgid_number(&self) -> PgidNumber {
        self.pgid
    }

    pub(crate) fn identity(&self) -> Arc<PidIdentity> {
        self.identity
            .upgrade()
            .expect("process group outlived its PID identity")
    }

    /// The [`Session`] that the [`ProcessGroup`] belongs to.
    pub fn session(&self) -> Arc<Session> {
        self.session.clone()
    }

    /// The [`Process`]es that belong to this [`ProcessGroup`].
    pub fn processes(&self) -> Vec<Arc<Process>> {
        self.processes.lock_irqsave().values().collect()
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

#[cfg(all(test, not(axtest)))]
mod tests {
    extern crate std;

    use std::{sync::Barrier, thread};

    use super::*;

    #[test]
    fn duplicate_live_group_identity_reuses_the_session_group() {
        let namespace = crate::task::new_test_pid_namespace();
        let (session_identity, _session_tgid) = crate::task::new_test_process_identity(&namespace);
        let session = Session::new(session_identity).unwrap();
        let (group_identity, _group_tgid) = crate::task::new_test_process_identity(&namespace);
        let start = Arc::new(Barrier::new(2));

        let first_session = session.clone();
        let first_start = start.clone();
        let first_identity = group_identity.clone();
        let first = thread::spawn(move || {
            first_start.wait();
            ProcessGroup::get_or_create(first_identity, &first_session).unwrap()
        });
        let second = thread::spawn(move || {
            start.wait();
            ProcessGroup::get_or_create(group_identity, &session).unwrap()
        });

        let first = first.join().unwrap();
        let second = second.join().unwrap();
        let session = first.session();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(session.process_groups().len(), 1);
    }
}
