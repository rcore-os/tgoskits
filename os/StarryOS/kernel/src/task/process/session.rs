use alloc::sync::{Arc, Weak};
#[cfg(test)]
use alloc::vec::Vec;
use core::{any::Any, fmt};

#[cfg(test)]
use super::ProcessGroup;
use super::{RelationLock, SessionGroups};
use crate::{
    StarryResult,
    task::{PidIdentity, PidRoleLease, Sid, SidNumber},
};

/// A [`Session`] is a collection of [`ProcessGroup`]s.
pub struct Session {
    sid: SidNumber,
    identity: Weak<PidIdentity>,
    _role: PidRoleLease<Sid>,
    pub(crate) process_groups: RelationLock<SessionGroups>,
    terminal: RelationLock<Option<Arc<dyn Any + Send + Sync>>>,
}

impl Session {
    /// Create a new [`Session`].
    pub(crate) fn new(identity: Arc<PidIdentity>) -> StarryResult<Arc<Self>> {
        let sid = SidNumber::from(identity.root_number());
        let role = identity.acquire_role::<Sid>()?;
        let session = Arc::new(Self {
            sid,
            identity: Arc::downgrade(&identity),
            _role: role,
            process_groups: RelationLock::new(SessionGroups::with_capacity(1)),
            terminal: RelationLock::new(None),
        });
        identity.bind_session(&session);
        Ok(session)
    }
}

impl Session {
    /// The root-namespace session ID.
    pub const fn sid(&self) -> SidNumber {
        self.sid
    }

    pub(crate) const fn sid_number(&self) -> SidNumber {
        self.sid
    }

    pub(crate) fn identity(&self) -> Arc<PidIdentity> {
        self.identity
            .upgrade()
            .expect("session outlived its PID identity")
    }

    /// The [`ProcessGroup`]s that belong to this [`Session`].
    #[cfg(test)]
    pub fn process_groups(&self) -> Vec<Arc<ProcessGroup>> {
        loop {
            let group_count = self.process_groups.lock().len();
            let mut groups = Vec::with_capacity(group_count);
            let relations = self.process_groups.lock();
            if groups.capacity() < relations.len() {
                drop(relations);
                continue;
            }
            relations.snapshot(&mut groups);
            return groups;
        }
    }

    /// Sets the terminal for this session with a fallible terminal initializer.
    pub fn try_set_terminal_with<E>(
        &self,
        terminal: impl FnOnce() -> Result<Arc<dyn Any + Send + Sync>, E>,
    ) -> Result<bool, E> {
        let mut guard = self.terminal.lock();
        if guard.is_some() {
            return Ok(false);
        }
        *guard = Some(terminal()?);
        Ok(true)
    }

    /// Unsets the terminal for this session if it is the given terminal.
    pub fn unset_terminal(&self, term: &Arc<dyn Any + Send + Sync>) -> bool {
        let mut guard = self.terminal.lock();
        if guard.as_ref().is_some_and(|it| Arc::ptr_eq(it, term)) {
            *guard = None;
            true
        } else {
            false
        }
    }

    /// Gets the terminal for this session, if it exists.
    pub fn terminal(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.terminal.lock().clone()
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Session({})", self.sid)
    }
}

#[cfg(axtest)]
pub(crate) fn duplicate_live_session_identity_is_rejected_for_test() -> bool {
    let namespace = crate::task::new_test_pid_namespace();
    let (identity, _tgid) = crate::task::new_test_process_identity(&namespace);
    let _session = Session::new(identity.clone()).unwrap();
    matches!(
        Session::new(identity),
        Err(crate::StarryError::AlreadyExists)
    )
}
