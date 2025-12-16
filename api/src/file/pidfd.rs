use alloc::{
    borrow::Cow,
    sync::{Arc, Weak},
};
use core::task::Context;

use axerrno::{AxError, AxResult};
use axpoll::{IoEvents, PollSet, Pollable};
use starry_core::task::ProcessData;

use crate::file::{FileLike, Kstat, SealedBuf, SealedBufMut};

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

    pub fn process_data(&self) -> AxResult<Arc<ProcessData>> {
        self.proc_data.upgrade().ok_or(AxError::NoSuchProcess)
    }
}
impl FileLike for PidFd {
    fn read(&self, _dst: &mut SealedBufMut) -> AxResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn write(&self, _src: &mut SealedBuf) -> AxResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat::default())
    }

    fn path(&self) -> Cow<'_, str> {
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
