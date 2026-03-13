use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{any::Any, fmt};

use kspin::SpinNoIrq;
use weak_map::WeakMap;

use crate::{Pid, ProcessGroup};

/// A [`Session`] is a collection of [`ProcessGroup`]s.
pub struct Session {
    sid: Pid,
    pub(crate) process_groups: SpinNoIrq<WeakMap<Pid, Weak<ProcessGroup>>>,
    terminal: SpinNoIrq<Option<Arc<dyn Any + Send + Sync>>>,
}

impl Session {
    /// Create a new [`Session`].
    pub(crate) fn new(sid: Pid) -> Arc<Self> {
        Arc::new(Self {
            sid,
            process_groups: SpinNoIrq::new(WeakMap::new()),
            terminal: SpinNoIrq::new(None),
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
        self.process_groups.lock().values().collect()
    }

    /// Sets the terminal for this session.
    pub fn set_terminal_with(&self, terminal: impl FnOnce() -> Arc<dyn Any + Send + Sync>) -> bool {
        let mut guard = self.terminal.lock();
        if guard.is_some() {
            return false;
        }
        *guard = Some(terminal());
        true
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
