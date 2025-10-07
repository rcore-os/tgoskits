use alloc::{
    borrow::Cow,
    collections::vec_deque::VecDeque,
    sync::{Arc, Weak},
    task::Wake,
};
use core::{
    any::Any,
    hash::{Hash, Hasher},
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Waker},
};

use axerrno::{AxError, AxResult};
use axpoll::{IoEvents, PollSet, Pollable};
use bitflags::bitflags;
use hashbrown::HashMap;
use kspin::SpinNoPreempt;
use linux_raw_sys::general::{EPOLLET, EPOLLONESHOT, epoll_event};

use crate::file::{FileLike, Kstat, SealedBuf, SealedBufMut, get_file_like};

type ReadyList = VecDeque<Weak<EpollInterest>>;

bitflags! {
    /// Flags for the entries in the `epoll` instance.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct EpollFlags: u32 {
        const EDGE_TRIGGER = EPOLLET;
        const ONESHOT = EPOLLONESHOT;
    }
}

pub struct EpollEvent {
    pub events: IoEvents,
    pub user_data: u64,
}

#[derive(Clone)]
struct EntryKey {
    fd: i32,
    file: Weak<dyn FileLike>,
}
impl EntryKey {
    fn new(fd: i32) -> AxResult<Self> {
        let file = get_file_like(fd)?;
        Ok(Self {
            fd,
            file: Arc::downgrade(&file),
        })
    }
}
impl Hash for EntryKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.fd, self.file.as_ptr()).hash(state);
    }
}
impl PartialEq for EntryKey {
    fn eq(&self, other: &Self) -> bool {
        self.fd == other.fd && Weak::ptr_eq(&self.file, &other.file)
    }
}
impl Eq for EntryKey {}

struct EntryWaker {
    ready: Weak<SpinNoPreempt<ReadyList>>,
    interest: Weak<EpollInterest>,
    poll_ready: Weak<PollSet>,
}
impl Wake for EntryWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(ready) = self.ready.upgrade()
            && let Some(interest) = self.interest.upgrade()
        {
            if interest.ready.swap(true, Ordering::AcqRel) {
                // already in ready list
                return;
            }
            ready.lock().push_back(Arc::downgrade(&interest));
            if let Some(poll_ready) = self.poll_ready.upgrade() {
                poll_ready.wake();
            }
        }
    }
}

struct EpollInterest {
    key: EntryKey,
    event: EpollEvent,
    flags: EpollFlags,
    enabled: AtomicBool,
    ready: AtomicBool,
}
impl EpollInterest {
    fn new(key: EntryKey, event: EpollEvent, flags: EpollFlags) -> Self {
        Self {
            key,
            event,
            flags,
            enabled: AtomicBool::new(true),
            ready: AtomicBool::new(false),
        }
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// Returns (`EpollEvent`, `still_ready`).
    fn poll(&self, file: &dyn FileLike) -> (Option<EpollEvent>, bool) {
        let events = file.poll();
        if events.intersects(self.event.events) {
            (
                Some(EpollEvent {
                    events,
                    user_data: self.event.user_data,
                }),
                !self
                    .flags
                    .intersects(EpollFlags::EDGE_TRIGGER | EpollFlags::ONESHOT),
            )
        } else {
            (None, false)
        }
    }
}

#[derive(Default)]
pub struct Epoll {
    interests: SpinNoPreempt<HashMap<EntryKey, Arc<EpollInterest>>>,
    ready: Arc<SpinNoPreempt<ReadyList>>,
    poll_ready: Arc<PollSet>,
}
impl Epoll {
    pub fn new() -> Self {
        Self::default()
    }

    fn repoll(&self, interest: &Arc<EpollInterest>) {
        if !interest.is_enabled() {
            return;
        }
        let Some(file) = interest.key.file.upgrade() else {
            return;
        };

        let waker = Waker::from(Arc::new(EntryWaker {
            ready: Arc::downgrade(&self.ready),
            interest: Arc::downgrade(interest),
            poll_ready: Arc::downgrade(&self.poll_ready),
        }));

        let (event, _) = interest.poll(file.as_ref());
        if event.is_some() {
            waker.wake_by_ref();
        } else {
            let mut context = Context::from_waker(&waker);
            file.register(&mut context, interest.event.events);
            // poll again after registering
            let (event, _) = interest.poll(file.as_ref());
            if event.is_some() {
                waker.wake_by_ref();
            }
        }
    }

    pub fn add(&self, fd: i32, event: EpollEvent, flags: EpollFlags) -> AxResult<()> {
        let key = EntryKey::new(fd)?;
        let mut guard = self.interests.lock();
        let interest = EpollInterest::new(key.clone(), event, flags);
        let interest = guard
            .try_insert(key.clone(), Arc::new(interest))
            .map_err(|_| AxError::AlreadyExists)?;
        self.repoll(interest);
        Ok(())
    }

    pub fn modify(&self, fd: i32, event: EpollEvent, flags: EpollFlags) -> AxResult<()> {
        let key = EntryKey::new(fd)?;
        let mut guard = self.interests.lock();
        let interest = guard.get_mut(&key).ok_or(AxError::NotFound)?;
        *interest = Arc::new(EpollInterest::new(key, event, flags));
        self.repoll(interest);
        Ok(())
    }

    pub fn delete(&self, fd: i32) -> AxResult<()> {
        let key = EntryKey::new(fd)?;
        self.interests
            .lock()
            .remove(&key)
            .map(|_| ())
            .ok_or(AxError::NotFound)
    }

    pub fn poll_events(&self, out: &mut [epoll_event]) -> AxResult<usize> {
        let mut ready = self.ready.lock();
        let mut result = 0;
        let len = ready.len();
        for _ in 0..len {
            let Some(slot) = out.get_mut(result) else {
                break;
            };
            let Some(interest) = ready.pop_front() else {
                break;
            };
            let Some(interest) = interest.upgrade() else {
                continue;
            };
            if !interest.is_enabled() {
                continue;
            }
            let Some(file) = interest.key.file.upgrade() else {
                // Remove the interest if the file is gone
                self.interests.lock().remove(&interest.key);
                continue;
            };
            let (event, still_ready) = interest.poll(file.as_ref());
            if let Some(event) = event {
                *slot = epoll_event {
                    events: event.events.bits(),
                    data: event.user_data,
                };
                result += 1;
                if interest.flags.contains(EpollFlags::ONESHOT) {
                    interest.enabled.store(false, Ordering::Release);
                    continue;
                }
            }
            if still_ready {
                ready.push_back(Arc::downgrade(&interest));
            } else {
                interest.ready.store(false, Ordering::Release);
                self.repoll(&interest);
            }
        }

        if result == 0 {
            Err(AxError::WouldBlock)
        } else {
            Ok(result)
        }
    }
}

impl FileLike for Epoll {
    fn read(&self, _dst: &mut SealedBufMut) -> AxResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn write(&self, _src: &mut SealedBuf) -> AxResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat::default())
    }

    fn path(&self) -> Cow<str> {
        "anon_inode:[eventpoll]".into()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl Pollable for Epoll {
    fn poll(&self) -> IoEvents {
        if self.ready.lock().is_empty() {
            IoEvents::empty()
        } else {
            IoEvents::IN
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_ready.register(context.waker());
        }
    }
}
