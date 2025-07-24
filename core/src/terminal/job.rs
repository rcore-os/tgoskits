use alloc::sync::{Arc, Weak};

use axerrno::{LinuxResult, bail};
use axprocess::{ProcessGroup, Session};
use axsync::spin::SpinNoIrq;
use axtask::{WaitQueue, current};

use crate::task::AsThread;

pub struct JobControl {
    foreground: SpinNoIrq<Weak<ProcessGroup>>,
    session: SpinNoIrq<Weak<Session>>,
    wait_queue: WaitQueue,
}
impl JobControl {
    pub fn new() -> Self {
        Self {
            foreground: SpinNoIrq::new(Weak::new()),
            session: SpinNoIrq::new(Weak::new()),
            wait_queue: WaitQueue::new(),
        }
    }

    pub fn current_in_foreground(&self) -> bool {
        self.foreground.lock().upgrade().map_or(true, |pg| {
            Arc::ptr_eq(&current().as_thread().proc_data.proc.group(), &pg)
        })
    }

    pub fn wait_until_foreground(&self) {
        self.wait_queue.wait_until(|| self.current_in_foreground())
    }

    pub fn foreground(&self) -> Option<Arc<ProcessGroup>> {
        self.foreground.lock().upgrade()
    }

    pub fn set_foreground(&self, pg: &Arc<ProcessGroup>) -> LinuxResult<()> {
        let mut guard = self.foreground.lock();
        let weak = Arc::downgrade(pg);
        if Weak::ptr_eq(&weak, &*guard) {
            return Ok(());
        }

        let Some(session) = self.session.lock().upgrade() else {
            bail!(EPERM, "No session associated with job control");
        };
        if !Arc::ptr_eq(&pg.session(), &session) {
            bail!(EPERM, "Process group does not belong to the session");
        }

        *guard = weak;
        drop(guard);
        self.wait_queue.notify_all(false);
        Ok(())
    }

    pub fn set_session(&self, session: &Arc<Session>) {
        let mut guard = self.session.lock();
        assert!(guard.upgrade().is_none());
        *guard = Arc::downgrade(session);
    }
}
