use alloc::{sync::Arc, vec::Vec};
use core::{any::Any, convert::Infallible, fmt};

use crate::{
    Pid, ProcessGroup,
    relations::{RelationLock, SessionGroups},
};

/// A [`Session`] is a collection of [`ProcessGroup`]s.
pub struct Session {
    sid: Pid,
    pub(crate) process_groups: RelationLock<SessionGroups>,
    // Terminal initialization can allocate and update TTY job-control state.
    // The multitask build therefore uses the same sleepable PI lock as process
    // relations instead of holding an IRQ spinlock across the initializer.
    terminal: RelationLock<Option<Arc<dyn Any + Send + Sync>>>,
}

impl Session {
    /// Create a new [`Session`].
    pub(crate) fn new(sid: Pid) -> Arc<Self> {
        Arc::new(Self {
            sid,
            process_groups: RelationLock::new(SessionGroups::with_capacity(1)),
            terminal: RelationLock::new(None),
        })
    }
}

impl Session {
    /// The [`Session`] ID.
    pub fn sid(&self) -> Pid {
        self.sid
    }

    /// The [`ProcessGroup`]s that belong to this [`Session`].
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
