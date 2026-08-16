use alloc::{borrow::Cow, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_task::AxTaskRef;
use axpoll::{IoEvents, PollSet, Pollable};

use crate::{
    StarryError, StarryResult,
    file::FileLike,
    task::{PidIdentity, Process, ProcessData, Thread, TidNumber},
};

pub struct PidFd {
    /// Stable generation addressed by this fd (TID for thread pidfds).
    identity: Arc<PidIdentity>,
    /// Stable thread-group generation used by process-scoped operations.
    process_identity: Arc<PidIdentity>,
    exit_event: Arc<PollSet>,
    thread_exit: Option<Arc<AtomicBool>>,
    tid: Option<TidNumber>,

    non_blocking: AtomicBool,
}
impl PidFd {
    pub(crate) fn new_process(identity: Arc<PidIdentity>) -> Self {
        Self {
            exit_event: identity.process_exit_event(),
            process_identity: identity.clone(),
            identity,
            thread_exit: None,
            tid: None,

            non_blocking: AtomicBool::new(false),
        }
    }

    pub(crate) fn new_thread(identity: Arc<PidIdentity>, thread: &Thread, tid: TidNumber) -> Self {
        Self {
            process_identity: thread.proc_data.identity(),
            identity,
            exit_event: thread.exit_event.clone(),
            thread_exit: Some(thread.exit.clone()),
            tid: Some(tid),

            non_blocking: AtomicBool::new(false),
        }
    }

    /// Creates a thread pidfd for an exited thread-group leader.
    pub(crate) fn new_exited_thread(identity: Arc<PidIdentity>) -> Self {
        let tid = TidNumber::from(identity.root_number());
        Self {
            exit_event: identity.process_exit_event(),
            process_identity: identity.clone(),
            identity,
            thread_exit: Some(Arc::new(AtomicBool::new(true))),
            tid: Some(tid),
            non_blocking: AtomicBool::new(false),
        }
    }

    pub fn is_thread(&self) -> bool {
        self.tid.is_some()
    }

    pub(crate) fn identity(&self) -> Arc<PidIdentity> {
        self.identity.clone()
    }

    pub(crate) fn process_identity(&self) -> Arc<PidIdentity> {
        self.process_identity.clone()
    }

    pub(crate) fn is_zombie(&self) -> bool {
        self.identity.is_zombie()
    }

    fn public_process(&self) -> StarryResult<Arc<Process>> {
        self.process_identity.public_process()
    }

    /// Resolves a process-scoped pidfd without requiring live runtime resources.
    pub fn signal_process(&self) -> StarryResult<Arc<Process>> {
        self.public_process()
    }

    /// Resolves a thread-scoped pidfd target.
    pub fn signal_thread(&self) -> StarryResult<AxTaskRef> {
        let tid = self.tid.ok_or(StarryError::InvalidInput)?;
        if self
            .thread_exit
            .as_ref()
            .is_some_and(|exited| exited.load(Ordering::Acquire))
            && !(tid.pid_number() == self.identity.root_number() && self.identity.is_zombie())
        {
            return Err(StarryError::NoSuchProcess);
        }
        self.identity.live_task().ok_or(StarryError::NoSuchProcess)
    }

    pub fn process_data(&self) -> StarryResult<Arc<ProcessData>> {
        // For threads, the pidfd is invalid once the thread exits, even if its
        // process is still alive.
        if let Some(thread_exit) = &self.thread_exit
            && thread_exit.load(Ordering::Acquire)
        {
            return Err(StarryError::NoSuchProcess);
        }
        self.process_identity
            .live_data()
            .ok_or(StarryError::NoSuchProcess)
    }
}
impl FileLike for PidFd {
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[pidfd]".into()
    }

    fn set_nonblocking(&self, nonblocking: bool) -> StarryResult {
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
            self.identity.process_poll_events()
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
