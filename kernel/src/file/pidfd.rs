use alloc::{
    borrow::Cow,
    sync::{Arc, Weak},
};
use core::task::Context;

use axerrno::{AxError, AxResult};
use axpoll::{IoEvents, PollSet, Pollable};

use crate::{file::FileLike, task::ProcessData};

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
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[pidfd]".into()
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
