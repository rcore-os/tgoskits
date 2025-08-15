use alloc::{borrow::Cow, sync::{Arc, Weak}};
use core::task::Context;

use axerrno::{LinuxError, LinuxResult};
use axio::{IoEvents, PollSet, Pollable};
use starry_core::task::ProcessData;

use crate::file::{FileLike, Kstat};

pub struct PidFd {
    proc_data: Weak<ProcessData>,
    exit_event: Arc<PollSet>,
}
impl PidFd {
    pub fn new(proc_data: &Arc<ProcessData>) -> Self {
        Self {
            proc_data: Arc::downgrade(proc_data),
            exit_event: proc_data.exit_event.clone(),
        }
    }

    pub fn process_data(&self) -> LinuxResult<Arc<ProcessData>> {
        self.proc_data.upgrade().ok_or(LinuxError::ESRCH)
    }
}
impl FileLike for PidFd {
    fn read(&self, _buf: &mut [u8]) -> LinuxResult<usize> {
        Err(LinuxError::EINVAL)
    }

    fn write(&self, _buf: &[u8]) -> LinuxResult<usize> {
        Err(LinuxError::EINVAL)
    }

    fn stat(&self) -> LinuxResult<Kstat> {
        Ok(Kstat::default())
    }

    fn path(&self) -> Cow<str> {
        "anon_inode:[pidfd]".into()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn core::any::Any + Send + Sync> {
        self
    }
}

impl Pollable for PidFd {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, self.proc_data.strong_count() > 0);
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.exit_event.register(context.waker());
        }
    }
}
