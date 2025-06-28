use alloc::sync::Arc;
use core::fmt;

use crate::{Pid, Process};

/// A thread.
pub struct Thread {
    pub(crate) tid: Pid,
    pub(crate) process: Arc<Process>,
}

impl Thread {
    /// The [`Thread`] ID.
    pub fn tid(&self) -> Pid {
        self.tid
    }

    /// The [`Process`] this thread belongs to.
    pub fn process(&self) -> &Arc<Process> {
        &self.process
    }

    /// Exits the thread.
    ///
    /// Returns `true` if the thread was the last one in the thread group.
    pub fn exit(&self, exit_code: i32) -> bool {
        let mut tg = self.process.tg.lock();
        if !tg.group_exited {
            tg.exit_code = exit_code;
        }
        tg.threads.remove(&self.tid);
        tg.threads.is_empty()
    }
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Thread({}, process={})", self.tid, self.process.pid())
    }
}
