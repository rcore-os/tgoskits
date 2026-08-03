use alloc::{borrow::Cow, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use axpoll::{IoEvents, PollSet, Pollable};
use starry_process::{Pid, Process};

use crate::{
    file::FileLike,
    task::{ProcessData, ProcessIdentity, Thread},
};

pub struct PidFd {
    identity: Arc<ProcessIdentity>,
    exit_event: Arc<PollSet>,
    thread_exit: Option<Arc<AtomicBool>>,
    tid: Option<Pid>,

    non_blocking: AtomicBool,
}
impl PidFd {
    pub(crate) fn new_process(identity: Arc<ProcessIdentity>) -> Self {
        Self {
            exit_event: identity.exit_event(),
            identity,
            thread_exit: None,
            tid: None,

            non_blocking: AtomicBool::new(false),
        }
    }

    pub(crate) fn new_thread(identity: Arc<ProcessIdentity>, thread: &Thread, tid: Pid) -> Self {
        Self {
            identity,
            exit_event: thread.exit_event.clone(),
            thread_exit: Some(thread.exit.clone()),
            tid: Some(tid),

            non_blocking: AtomicBool::new(false),
        }
    }

    /// Creates a thread pidfd for an exited thread-group leader.
    pub(crate) fn new_exited_thread(identity: Arc<ProcessIdentity>) -> Self {
        let pid = identity.pid();
        Self {
            exit_event: identity.exit_event(),
            identity,
            thread_exit: Some(Arc::new(AtomicBool::new(true))),
            tid: Some(pid),
            non_blocking: AtomicBool::new(false),
        }
    }

    pub fn is_thread(&self) -> bool {
        self.tid.is_some()
    }

    pub fn process_pid(&self) -> Pid {
        self.identity.pid()
    }

    pub(crate) fn target_pid(&self) -> Pid {
        self.tid.unwrap_or_else(|| self.identity.pid())
    }

    pub(crate) fn identity(&self) -> Arc<ProcessIdentity> {
        self.identity.clone()
    }

    pub(crate) fn is_zombie(&self) -> bool {
        self.identity.is_zombie()
    }

    fn public_process(&self) -> AxResult<Arc<Process>> {
        self.identity.public_process()
    }

    /// Resolves a process-scoped pidfd without requiring live runtime resources.
    pub fn signal_process(&self) -> AxResult<Arc<Process>> {
        self.public_process()
    }

    /// Resolves a thread-scoped pidfd target.
    pub fn signal_thread(&self) -> AxResult<(Arc<Process>, Pid)> {
        let tid = self.tid.ok_or(AxError::InvalidInput)?;
        if self
            .thread_exit
            .as_ref()
            .is_some_and(|exited| exited.load(Ordering::Acquire))
            && !(tid == self.identity.pid() && self.identity.is_zombie())
        {
            return Err(AxError::NoSuchProcess);
        }
        Ok((self.public_process()?, tid))
    }

    pub fn process_data(&self) -> AxResult<Arc<ProcessData>> {
        // For threads, the pidfd is invalid once the thread exits, even if its
        // process is still alive.
        if let Some(thread_exit) = &self.thread_exit
            && thread_exit.load(Ordering::Acquire)
        {
            return Err(AxError::NoSuchProcess);
        }
        self.identity.live_data().ok_or(AxError::NoSuchProcess)
    }
}
impl FileLike for PidFd {
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[pidfd]".into()
    }

    fn set_nonblocking(&self, nonblocking: bool) -> AxResult {
        self.non_blocking.store(nonblocking, Ordering::Release);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }
}

impl Pollable for PidFd {
    fn poll(&self) -> IoEvents {
        // Linux pidfd becomes readable only after the referenced task exits.
        // Reporting IN while it is still alive makes event loops spin or wait
        // on the wrong readiness edge.
        if let Some(thread_exit) = &self.thread_exit {
            let exited = thread_exit.load(Ordering::Acquire);
            let mut events = if exited {
                IoEvents::IN | IoEvents::RDNORM
            } else {
                IoEvents::empty()
            };
            events.set(IoEvents::HUP, self.identity.is_reaped());
            events
        } else {
            self.identity.poll_events()
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        let interests = events & (IoEvents::IN | IoEvents::RDNORM | IoEvents::HUP);
        if !interests.is_empty() {
            // Registration happens from pidfd poll task context.
            unsafe { self.exit_event.register(context.waker(), interests) };
        }
    }
}
