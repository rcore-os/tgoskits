use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{any::Any, convert::Infallible, fmt};

use weak_map::WeakMap;

use super::ProcessGroup;
use crate::{
    StarryResult,
    sync::SpinLock,
    task::{PgidNumber, PidIdentity, PidRoleLease, Sid, SidNumber},
};

/// A [`Session`] is a collection of [`ProcessGroup`]s.
pub struct Session {
    sid: SidNumber,
    identity: Weak<PidIdentity>,
    _role: PidRoleLease<Sid>,
    pub(crate) process_groups: SpinLock<WeakMap<PgidNumber, Weak<ProcessGroup>>>,
    terminal: SpinLock<Option<Arc<dyn Any + Send + Sync>>>,
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
            process_groups: SpinLock::new(WeakMap::new()),
            terminal: SpinLock::new(None),
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
    pub fn process_groups(&self) -> Vec<Arc<ProcessGroup>> {
        self.process_groups.lock_irqsave().values().collect()
    }

    /// Sets the terminal for this session.
    pub fn set_terminal_with(&self, terminal: impl FnOnce() -> Arc<dyn Any + Send + Sync>) -> bool {
        self.try_set_terminal_with(|| Ok::<_, Infallible>(terminal()))
            .unwrap()
    }

    /// Sets the terminal for this session with a fallible terminal initializer.
    pub fn try_set_terminal_with<E>(
        &self,
        terminal: impl FnOnce() -> Result<Arc<dyn Any + Send + Sync>, E>,
    ) -> Result<bool, E> {
        let mut guard = self.terminal.lock_irqsave();
        if guard.is_some() {
            return Ok(false);
        }
        *guard = Some(terminal()?);
        Ok(true)
    }

    /// Unsets the terminal for this session if it is the given terminal.
    pub fn unset_terminal(&self, term: &Arc<dyn Any + Send + Sync>) -> bool {
        let mut guard = self.terminal.lock_irqsave();
        if guard.as_ref().is_some_and(|it| Arc::ptr_eq(it, term)) {
            *guard = None;
            true
        } else {
            false
        }
    }

    /// Gets the terminal for this session, if it exists.
    pub fn terminal(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.terminal.lock_irqsave().clone()
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Session({})", self.sid)
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn duplicate_live_session_identity_is_rejected() {
        let namespace = crate::task::new_test_pid_namespace();
        let (identity, _tgid) = crate::task::new_test_process_identity(&namespace);
        let _session = Session::new(identity.clone()).unwrap();
        assert!(matches!(
            Session::new(identity),
            Err(crate::StarryError::AlreadyExists)
        ));
    }
}
