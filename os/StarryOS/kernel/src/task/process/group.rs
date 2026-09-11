use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::fmt;

use super::{GroupMembers, Process, RelationLock, Session, ensure_session_capacity};
use crate::{
    StarryResult,
    task::{Pgid, PgidNumber, PidIdentity, PidRoleLease},
};

/// A [`ProcessGroup`] is a collection of [`Process`]es.
pub struct ProcessGroup {
    pgid: PgidNumber,
    identity: Weak<PidIdentity>,
    _role: PidRoleLease<Pgid>,
    pub(crate) session: Arc<Session>,
    pub(crate) processes: RelationLock<GroupMembers>,
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
        loop {
            ensure_session_capacity(&session.process_groups, 1);
            let mut groups = session.process_groups.lock();
            if let Some(existing) = groups.get_live(pgid.pid_number()) {
                return Ok(existing);
            }
            if !groups.has_capacity_for(1) {
                drop(groups);
                continue;
            }
            let role = identity.acquire_role::<Pgid>()?;
            let group = Arc::new(Self {
                pgid,
                identity: Arc::downgrade(&identity),
                _role: role,
                session: session.clone(),
                processes: RelationLock::new(GroupMembers::with_capacity(1)),
            });
            identity.bind_process_group(&group);
            let replaced = groups.insert_reserved(pgid.pid_number(), &group);
            debug_assert!(replaced.is_none());
            drop(groups);
            drop(replaced);
            return Ok(group);
        }
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
            self.pgid(),
            self.session.sid()
        )
    }
}

#[cfg(all(test, axtest))]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct TestBarrier {
        arrivals: AtomicUsize,
        participants: usize,
    }

    impl TestBarrier {
        const fn new(participants: usize) -> Self {
            Self {
                arrivals: AtomicUsize::new(0),
                participants,
            }
        }

        fn wait(&self) {
            self.arrivals.fetch_add(1, Ordering::Release);
            while self.arrivals.load(Ordering::Acquire) < self.participants {
                ax_std::thread::yield_now();
            }
        }
    }

    #[axtest::axtest]
    fn duplicate_live_group_identity_reuses_the_session_group() {
        let namespace = crate::task::new_test_pid_namespace();
        let (session_identity, _session_tgid) = crate::task::new_test_process_identity(&namespace);
        let session = Session::new(session_identity).unwrap();
        let (group_identity, _group_tgid) = crate::task::new_test_process_identity(&namespace);
        let start = Arc::new(TestBarrier::new(2));

        let first_session = session.clone();
        let first_start = start.clone();
        let first_identity = group_identity.clone();
        let first = ax_std::thread::spawn(move || {
            first_start.wait();
            ProcessGroup::get_or_create(first_identity, &first_session).unwrap()
        });
        let second = ax_std::thread::spawn(move || {
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
