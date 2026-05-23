use alloc::{
    borrow::Cow,
    collections::{BTreeMap, VecDeque},
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    task::Context,
};

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
use ax_task::future::{block_on, poll_io};
use axpoll::{IoEvents, PollSet, Pollable};
use lazy_static::lazy_static;
use linux_raw_sys::{
    general::{IN_IGNORED, IN_MODIFY},
    ioctl::FIONREAD,
};
use starry_vm::VmMutPtr;

use crate::file::{FileLike, IoDst, IoSrc};

const INOTIFY_EVENT_SIZE: usize = 16;
const MAX_QUEUED_EVENTS: usize = 1024;

#[derive(Clone)]
struct Watch {
    path: String,
    mask: u32,
}

#[derive(Default)]
struct InotifyState {
    next_wd: i32,
    watches: BTreeMap<i32, Watch>,
    queue: VecDeque<Vec<u8>>,
}

pub struct Inotify {
    non_blocking: AtomicBool,
    state: Mutex<InotifyState>,
    poll_rx: PollSet,
}

lazy_static! {
    static ref INOTIFY_INSTANCES: Mutex<Vec<Weak<Inotify>>> = Mutex::new(Vec::new());
}

impl Inotify {
    pub fn new() -> Arc<Self> {
        let inotify = Arc::new(Self {
            non_blocking: AtomicBool::new(false),
            state: Mutex::new(InotifyState {
                next_wd: 1,
                ..InotifyState::default()
            }),
            poll_rx: PollSet::new(),
        });
        INOTIFY_INSTANCES.lock().push(Arc::downgrade(&inotify));
        inotify
    }

    pub fn add_watch(&self, path: String, mask: u32) -> AxResult<i32> {
        if mask == 0 {
            return Err(AxError::InvalidInput);
        }

        let mut state = self.state.lock();
        if let Some((wd, watch)) = state
            .watches
            .iter_mut()
            .find(|(_, watch)| watch.path == path)
        {
            watch.mask = mask;
            return Ok(*wd);
        }

        let wd = state.next_wd;
        state.next_wd = state.next_wd.checked_add(1).ok_or(AxError::NoMemory)?;
        state.watches.insert(wd, Watch { path, mask });
        Ok(wd)
    }

    pub fn rm_watch(&self, wd: i32) -> AxResult {
        let mut state = self.state.lock();
        if state.watches.remove(&wd).is_none() {
            return Err(AxError::InvalidInput);
        }
        Self::push_event(&mut state.queue, wd, IN_IGNORED);
        self.poll_rx.wake();
        Ok(())
    }

    fn notify_modify(&self, path: &str) {
        let mut state = self.state.lock();
        let matched_wds = state
            .watches
            .iter()
            .filter_map(|(wd, watch)| {
                if watch.path == path && watch.mask & IN_MODIFY != 0 {
                    Some(*wd)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        if matched_wds.is_empty() {
            return;
        }
        for wd in matched_wds {
            Self::push_event(&mut state.queue, wd, IN_MODIFY);
        }
        self.poll_rx.wake();
    }

    fn push_event(queue: &mut VecDeque<Vec<u8>>, wd: i32, mask: u32) {
        if queue.len() >= MAX_QUEUED_EVENTS {
            queue.pop_front();
        }

        let mut event = Vec::with_capacity(INOTIFY_EVENT_SIZE);
        event.extend_from_slice(&wd.to_ne_bytes());
        event.extend_from_slice(&mask.to_ne_bytes());
        event.extend_from_slice(&0u32.to_ne_bytes());
        event.extend_from_slice(&0u32.to_ne_bytes());
        queue.push_back(event);
    }
}

impl FileLike for Inotify {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        if dst.remaining_mut() < INOTIFY_EVENT_SIZE {
            return Err(AxError::InvalidInput);
        }

        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            let mut state = self.state.lock();
            let mut written = 0;
            while let Some(event) = state.queue.front() {
                if dst.remaining_mut() < event.len() {
                    break;
                }
                written += dst.write(event)?;
                state.queue.pop_front();
            }
            if written == 0 {
                Err(AxError::WouldBlock)
            } else {
                Ok(written)
            }
        }))
    }

    fn write(&self, _src: &mut IoSrc) -> AxResult<usize> {
        Err(AxError::BadFileDescriptor)
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, non_blocking: bool) -> AxResult {
        self.non_blocking.store(non_blocking, Ordering::Release);
        Ok(())
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[inotify]".into()
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        match cmd {
            FIONREAD => {
                let pending = self
                    .state
                    .lock()
                    .queue
                    .iter()
                    .map(Vec::len)
                    .sum::<usize>()
                    .min(u32::MAX as usize) as u32;
                (arg as *mut u32).vm_write(pending)?;
                Ok(0)
            }
            _ => Err(AxError::NotATty),
        }
    }
}

impl Pollable for Inotify {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, !self.state.lock().queue.is_empty());
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_rx.register(context.waker());
        }
    }
}

pub fn notify_modify_path(path: &str) {
    if path == "<error>" {
        return;
    }

    let mut instances = INOTIFY_INSTANCES.lock();
    instances.retain(|watcher| {
        if let Some(inotify) = watcher.upgrade() {
            inotify.notify_modify(path);
            true
        } else {
            false
        }
    });
}
