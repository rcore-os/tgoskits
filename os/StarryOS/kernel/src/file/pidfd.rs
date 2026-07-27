use alloc::{
    borrow::Cow,
    sync::{Arc, Weak},
};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use axpoll::{IoEvents, PollSet, Pollable};
use starry_process::{Pid, Process};

use crate::{
    file::FileLike,
    task::{ProcessData, Thread, get_process, get_process_data},
};

pub struct PidFd {
    pid: Pid,
    process: Arc<Process>,
    proc_data: Weak<ProcessData>,
    exit_event: Arc<PollSet>,
    thread_exit: Option<Arc<AtomicBool>>,
    tid: Option<Pid>,

    non_blocking: AtomicBool,
}
impl PidFd {
    pub fn new_process(proc_data: &Arc<ProcessData>) -> Self {
        Self {
            pid: proc_data.proc.pid(),
            process: proc_data.proc.clone(),
            proc_data: Arc::downgrade(proc_data),
            exit_event: proc_data.exit_event.clone(),
            thread_exit: None,
            tid: None,

            non_blocking: AtomicBool::new(false),
        }
    }

    pub fn new_thread(thread: &Thread, tid: Pid) -> Self {
        Self {
            pid: tid,
            process: thread.proc_data.proc.clone(),
            proc_data: Arc::downgrade(&thread.proc_data),
            exit_event: thread.exit_event.clone(),
            thread_exit: Some(thread.exit.clone()),
            tid: Some(tid),

            non_blocking: AtomicBool::new(false),
        }
    }

    /// Creates a pidfd for a process that exited but has not been reaped.
    pub fn new_exited_process(process: Arc<Process>, exit_event: Arc<PollSet>) -> Self {
        Self {
            pid: process.pid(),
            process,
            proc_data: Weak::new(),
            exit_event,
            thread_exit: None,
            tid: None,
            non_blocking: AtomicBool::new(false),
        }
    }

    /// Creates a thread pidfd for an exited thread-group leader.
    pub fn new_exited_thread(process: Arc<Process>, exit_event: Arc<PollSet>) -> Self {
        let pid = process.pid();
        Self {
            pid,
            process,
            proc_data: Weak::new(),
            exit_event,
            thread_exit: Some(Arc::new(AtomicBool::new(true))),
            tid: Some(pid),
            non_blocking: AtomicBool::new(false),
        }
    }

    pub fn is_thread(&self) -> bool {
        self.tid.is_some()
    }

    pub fn pid(&self) -> Pid {
        self.pid
    }

    pub fn process_pid(&self) -> Pid {
        self.process.pid()
    }

    fn registered_process(&self) -> AxResult<Arc<Process>> {
        let registered = get_process(self.process.pid())?;
        if !Arc::ptr_eq(&registered, &self.process) {
            return Err(AxError::NoSuchProcess);
        }
        Ok(registered)
    }

    /// Resolves a process-scoped pidfd without requiring live runtime resources.
    pub fn signal_process(&self) -> AxResult<Arc<Process>> {
        self.registered_process()
    }

    /// Resolves a thread-scoped pidfd target.
    pub fn signal_thread(&self) -> AxResult<(Arc<Process>, Pid)> {
        let tid = self.tid.ok_or(AxError::InvalidInput)?;
        if self
            .thread_exit
            .as_ref()
            .is_some_and(|exited| exited.load(Ordering::Acquire))
            && !(tid == self.process.pid() && self.process.is_zombie())
        {
            return Err(AxError::NoSuchProcess);
        }
        Ok((self.registered_process()?, tid))
    }

    pub fn process_data(&self) -> AxResult<Arc<ProcessData>> {
        // For threads, the pidfd is invalid once the thread exits, even if its
        // process is still alive.
        if let Some(thread_exit) = &self.thread_exit
            && thread_exit.load(Ordering::Acquire)
        {
            return Err(AxError::NoSuchProcess);
        }
        let proc_data = self.proc_data.upgrade().ok_or(AxError::NoSuchProcess)?;
        // `ProcessData` may outlive `waitpid` while the pid is no longer in
        // `PROCESS_TABLE`. Linux pidfd ops on a reaped pid return ESRCH instead
        // of falling through to EBADF from an empty fd table.
        let registered = get_process_data(proc_data.proc.pid())?;
        if !Arc::ptr_eq(&registered, &proc_data) {
            return Err(AxError::NoSuchProcess);
        }
        Ok(proc_data)
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
        let exited = if let Some(thread_exit) = &self.thread_exit {
            thread_exit.load(Ordering::Acquire)
        } else {
            self.process.is_zombie() || self.process.is_reaped()
        };
        let mut events = if exited {
            IoEvents::IN | IoEvents::RDNORM
        } else {
            IoEvents::empty()
        };
        events.set(IoEvents::HUP, self.process.is_reaped());
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        let interests = events & (IoEvents::IN | IoEvents::RDNORM | IoEvents::HUP);
        if !interests.is_empty() {
            // Registration happens from pidfd poll task context.
            unsafe { self.exit_event.register(context.waker(), interests) };
        }
    }
}
