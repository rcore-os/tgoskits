use alloc::sync::{Arc, Weak};
use core::task::Context;

use ax_errno::{AxResult, ax_bail};
use ax_kspin::SpinNoIrq;
use ax_task::current;
use axpoll::{IoEvents, PollSet, Pollable};
use starry_process::{ProcessGroup, Session};

use crate::task::AsThread;

pub struct JobControl {
    foreground: SpinNoIrq<Weak<ProcessGroup>>,
    session: SpinNoIrq<Weak<Session>>,
    poll_fg: PollSet,
}

impl Default for JobControl {
    fn default() -> Self {
        Self::new()
    }
}

impl JobControl {
    pub fn new() -> Self {
        Self {
            foreground: SpinNoIrq::new(Weak::new()),
            session: SpinNoIrq::new(Weak::new()),
            poll_fg: PollSet::new(),
        }
    }

    pub fn current_in_foreground(&self) -> bool {
        self.foreground
            .lock()
            .upgrade()
            .is_none_or(|pg| Arc::ptr_eq(&current().as_thread().proc_data.proc.group(), &pg))
    }

    pub fn foreground(&self) -> Option<Arc<ProcessGroup>> {
        self.foreground.lock().upgrade()
    }

    pub fn set_foreground(&self, pg: &Arc<ProcessGroup>) -> AxResult<()> {
        let mut guard = self.foreground.lock();
        let weak = Arc::downgrade(pg);
        if Weak::ptr_eq(&weak, &*guard) {
            return Ok(());
        }

        let Some(session) = self.session.lock().upgrade() else {
            ax_bail!(
                OperationNotPermitted,
                "No session associated with job control"
            );
        };
        if !Arc::ptr_eq(&pg.session(), &session) {
            ax_bail!(
                OperationNotPermitted,
                "Process group does not belong to the session"
            );
        }

        *guard = weak;
        drop(guard);
        self.poll_fg.wake();
        Ok(())
    }

    pub fn set_session(&self, session: &Arc<Session>) -> AxResult<()> {
        let mut guard = self.session.lock();
        if let Some(bound) = guard.upgrade() {
            if Arc::ptr_eq(&bound, session) {
                return Ok(());
            }
            ax_bail!(
                OperationNotPermitted,
                "Terminal is already associated with another session"
            );
        }
        *guard = Arc::downgrade(session);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use starry_process::Process;

    use super::*;

    #[test]
    fn setting_different_session_returns_eperm() {
        let init = Process::new_init(1);
        let first = init.fork(2);
        let second = init.fork(3);
        let (first_session, _) = first.create_session().unwrap();
        let (second_session, _) = second.create_session().unwrap();
        let jobs = JobControl::new();

        jobs.set_session(&first_session).unwrap();

        assert!(jobs.set_session(&second_session).is_err());
    }
}

impl Pollable for JobControl {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, self.current_in_foreground());
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_fg.register(context.waker());
        }
    }
}
