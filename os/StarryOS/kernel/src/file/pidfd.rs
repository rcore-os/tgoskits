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
use starry_process::Pid;

use crate::{
    file::FileLike,
    task::{ProcessData, Thread, get_process_data},
};

pub struct PidFd {
    pid: Pid,
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
            proc_data: Arc::downgrade(&thread.proc_data),
            exit_event: thread.exit_event.clone(),
            thread_exit: Some(thread.exit.clone()),
            tid: Some(tid),

            non_blocking: AtomicBool::new(false),
        }
    }

    pub fn is_thread(&self) -> bool {
        self.tid.is_some()
    }

    pub fn pid(&self) -> Pid {
        self.pid
    }

    pub fn tid(&self) -> Option<Pid> {
        self.tid
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
        get_process_data(proc_data.proc.pid())?;
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
        let mut events = IoEvents::empty();
        events.set(
            IoEvents::IN,
            self.proc_data.strong_count() > 0
                && self
                    .thread_exit
                    .as_ref()
                    .is_none_or(|it| !it.load(Ordering::Acquire)),
        );
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            // Registration happens from pidfd poll task context.
            unsafe { self.exit_event.register(context.waker(), IoEvents::IN) };
        }
    }
}
