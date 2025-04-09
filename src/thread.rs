use alloc::{boxed::Box, sync::Arc};
use core::{any::Any, fmt};

use crate::{Pid, Process};

/// A thread.
pub struct Thread {
    tid: Pid,
    process: Arc<Process>,
    data: Box<dyn Any + Send + Sync>,
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

    /// The data associated with the [`Thread`].
    pub fn data<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.data.downcast_ref::<T>()
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
        f.debug_struct("Thread")
            .field("tid", &self.tid)
            .field("process", &self.process.pid())
            .finish()
    }
}

/// A builder for creating a new [`Thread`].
pub struct ThreadBuilder {
    tid: Pid,
    process: Arc<Process>,
    data: Box<dyn Any + Send + Sync>,
}

impl ThreadBuilder {
    pub(crate) fn new(tid: Pid, process: Arc<Process>) -> Self {
        Self {
            tid,
            process,
            data: Box::new(()),
        }
    }

    /// Sets the data associated with the [`Thread`].
    pub fn data<T: Any + Send + Sync>(self, data: T) -> Self {
        Self {
            data: Box::new(data),
            ..self
        }
    }

    /// Builds the [`Thread`].
    pub fn build(self) -> Arc<Thread> {
        let Self { tid, process, data } = self;

        let thread = Arc::new(Thread {
            tid,
            process: process.clone(),
            data,
        });
        process.tg.lock().threads.insert(tid, &thread);

        thread
    }
}
