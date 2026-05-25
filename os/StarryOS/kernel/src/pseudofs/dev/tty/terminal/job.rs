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
        if let Some(existing) = guard.upgrade() {
            if Arc::ptr_eq(&existing, session) {
                return Ok(());
            }
            ax_bail!(
                ResourceBusy,
                "Terminal is already associated with another session"
            );
        }
        *guard = Arc::downgrade(session);
        Ok(())
    }

    pub fn clear_session(&self, session: &Arc<Session>) {
        {
            let mut session_guard = self.session.lock();
            if session_guard
                .upgrade()
                .is_some_and(|existing| Arc::ptr_eq(&existing, session))
            {
                *session_guard = Weak::new();
            }
        }

        let mut foreground_guard = self.foreground.lock();
        if foreground_guard
            .upgrade()
            .is_some_and(|pg| Arc::ptr_eq(&pg.session(), session))
        {
            *foreground_guard = Weak::new();
            drop(foreground_guard);
            self.poll_fg.wake();
        }
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
