use alloc::sync::{Arc, Weak};
use core::task::Context;

use ax_errno::{AxResult, ax_bail};
use ax_kspin::SpinNoIrq;
use axpoll::{IoEvents, PollSet, Pollable};
use starry_process::{ProcessGroup, Session};

use crate::task::current_user_task;

pub struct JobControl {
    state: SpinNoIrq<JobControlState>,
    poll_fg: PollSet,
}

struct JobControlState {
    foreground: Weak<ProcessGroup>,
    session: Weak<Session>,
}

impl Default for JobControl {
    fn default() -> Self {
        Self::new()
    }
}

impl JobControl {
    pub fn new() -> Self {
        Self {
            state: SpinNoIrq::new(JobControlState {
                foreground: Weak::new(),
                session: Weak::new(),
            }),
            poll_fg: PollSet::new(),
        }
    }

    pub fn current_in_foreground(&self) -> bool {
        self.state.lock().foreground.upgrade().is_none_or(|pg| {
            Arc::ptr_eq(&current_user_task().as_thread().proc_data.proc.group(), &pg)
        })
    }

    pub fn foreground(&self) -> Option<Arc<ProcessGroup>> {
        self.state.lock().foreground.upgrade()
    }

    pub fn set_foreground(&self, pg: &Arc<ProcessGroup>) -> AxResult<()> {
        let mut state = self.state.lock();
        let weak = Arc::downgrade(pg);
        if Weak::ptr_eq(&weak, &state.foreground) {
            return Ok(());
        }

        let Some(session) = state.session.upgrade() else {
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

        state.foreground = weak;
        drop(state);
        // Foreground process-group state is published before waking waiters.
        unsafe { self.poll_fg.wake(IoEvents::IN) };
        Ok(())
    }

    pub fn set_session(&self, session: &Arc<Session>) -> AxResult<()> {
        let mut state = self.state.lock();
        if let Some(existing) = state.session.upgrade() {
            if Arc::ptr_eq(&existing, session) {
                return Ok(());
            }
            ax_bail!(
                ResourceBusy,
                "Terminal is already associated with another session"
            );
        }
        state.session = Arc::downgrade(session);
        Ok(())
    }

    pub fn clear_session(&self, session: &Arc<Session>) {
        let mut state = self.state.lock();
        if state
            .session
            .upgrade()
            .is_some_and(|existing| Arc::ptr_eq(&existing, session))
        {
            state.session = Weak::new();
        }

        let foreground_cleared = state
            .foreground
            .upgrade()
            .is_some_and(|pg| Arc::ptr_eq(&pg.session(), session));
        if foreground_cleared {
            state.foreground = Weak::new();
        }
        drop(state);

        if foreground_cleared {
            // Foreground process-group state is published before waking waiters.
            unsafe { self.poll_fg.wake(IoEvents::IN) };
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
            // Registration happens from tty job-control poll task context.
            unsafe { self.poll_fg.register(context.waker(), IoEvents::IN) };
        }
    }
}
