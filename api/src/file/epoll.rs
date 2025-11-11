// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2025 KylinSoft Co., Ltd. <https://www.kylinos.cn/>
// Copyright (C) 2025 Azure-stars <Azure_stars@126.com>
// Copyright (C) 2025 Yuekai Jia <equation618@gmail.com>
// See LICENSES for license details.
// 
// This file has been modified by KylinSoft on 2025.

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

pub struct EpollEvent {
    pub events: IoEvents,
    pub user_data: u64,
}

bitflags! {
    /// Flags for the entries in the `epoll` instance.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct EpollFlags: u32 {
        const EDGE_TRIGGER = EPOLLET;
        const ONESHOT = EPOLLONESHOT;
    }
}

/// Interest trigger mode
#[derive(Debug, Clone, Copy)]
enum TriggerMode {
    /// Level-triggered: until the condition is cleared
    Level,
    /// Edge-triggered: only notify when the condition changes
    Edge,
    /// One-shot: notify only once
    OneShot { fired: bool },
}

impl TriggerMode {
    fn from_flags(flags: EpollFlags) -> Self {
        if flags.contains(EpollFlags::ONESHOT) {
            TriggerMode::OneShot { fired: false }
        } else if flags.contains(EpollFlags::EDGE_TRIGGER) {
            TriggerMode::Edge
        } else {
            TriggerMode::Level
        }
    }

    // return should notify and new mode
    fn should_notify(&self) -> (bool, Self) {
        match self {
            TriggerMode::Level => {
                // LT: always notify
                (true, *self)
            }
            // if we could wake, we need notify
            TriggerMode::Edge => (true, TriggerMode::Edge),
            TriggerMode::OneShot { fired } => {
                // ONESHOT: 只触发一次
                if *fired {
                    (false, *self)
                } else {
                    (true, TriggerMode::OneShot { fired: true })
                }
            }
        }
    }

    fn is_enabled(&self) -> bool {
        match self {
            TriggerMode::OneShot { fired } => !fired,
            _ => true,
        }
    }
}

enum ConsumeResult {
    // success and should keep in ready list
    EventAndKeep(EpollEvent),
    // success and hould remove ready list
    EventAndRemove(EpollEvent),
    // no event and should remove ready list
    NoEvent,
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

    #[inline]
    fn get_file(&self) -> Option<Arc<dyn FileLike>> {
        self.file.upgrade()
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

struct EpollInterest {
    key: EntryKey,
    event: EpollEvent,
    mode: SpinNoPreempt<TriggerMode>,
    in_ready_queue: AtomicBool,
}

impl EpollInterest {
    fn new(key: EntryKey, event: EpollEvent, flags: EpollFlags) -> Self {
        Self {
            key,
            event,
            mode: SpinNoPreempt::new(TriggerMode::from_flags(flags)),
            in_ready_queue: AtomicBool::new(false),
        }
    }

    #[inline]
    fn is_enabled(&self) -> bool {
        self.mode.lock().is_enabled()
    }

    #[inline]
    fn is_in_queue(&self) -> bool {
        self.in_ready_queue.load(Ordering::Acquire)
    }

    #[inline]
    fn try_mark_in_queue(&self) -> bool {
        self.in_ready_queue
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    #[inline]
    fn mark_not_in_queue(&self) {
        self.in_ready_queue.store(false, Ordering::Release);
    }

    fn consume(&self, file: &dyn FileLike) -> ConsumeResult {
        let current_events = file.poll();
        let matched = current_events & self.event.events;

        // not ready
        if matched.is_empty() {
            return ConsumeResult::NoEvent;
        }

        let mut mode = self.mode.lock();
        let (should_notify, new_mode) = mode.should_notify();
        *mode = new_mode;
        trace!(
            "consume fd: {} matches {:?} should notify: {} ",
            self.key.fd, matched, should_notify
        );

        if !should_notify {
            return ConsumeResult::NoEvent;
        }

        // create event
        let event = EpollEvent {
            events: matched,
            user_data: self.event.user_data,
        };

        // shoud still keep in ready?
        match *mode {
            TriggerMode::Level => ConsumeResult::EventAndKeep(event),
            TriggerMode::Edge | TriggerMode::OneShot { .. } => ConsumeResult::EventAndRemove(event),
        }
    }
}

struct InterestWaker {
    epoll: Weak<EpollInner>,
    interest: Weak<EpollInterest>,
}

impl Wake for InterestWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let Some(epoll) = self.epoll.upgrade() else {
            return;
        };

        let Some(interest) = self.interest.upgrade() else {
            return;
        };

        if interest.try_mark_in_queue() {
            epoll
                .ready_queue
                .lock()
                .push_back(Arc::downgrade(&interest));
            trace!(
                "Epoll: fd={} added to ready queue, events={:?} wake up poller",
                interest.key.fd, interest.event.events
            );
            epoll.poll_ready.wake();
        }
    }
}

struct EpollInner {
    interests: SpinNoPreempt<HashMap<EntryKey, Arc<EpollInterest>>>,
    ready_queue: SpinNoPreempt<VecDeque<Weak<EpollInterest>>>,
    poll_ready: PollSet,
}

impl Default for EpollInner {
    fn default() -> Self {
        Self {
            interests: SpinNoPreempt::new(HashMap::new()),
            ready_queue: SpinNoPreempt::new(VecDeque::new()),
            poll_ready: PollSet::new(),
        }
    }
}

#[derive(Default)]
pub struct Epoll {
    inner: Arc<EpollInner>,
}

impl Epoll {
    pub fn new() -> Self {
        Self::default()
    }

    // only register waker, not add to ready queue
    fn register_waker_only(&self, interest: &Arc<EpollInterest>) {
        let Some(file) = interest.key.get_file() else {
            return;
        };

        if !interest.is_enabled() {
            return;
        }

        let waker = Waker::from(Arc::new(InterestWaker {
            epoll: Arc::downgrade(&self.inner),
            interest: Arc::downgrade(interest),
        }));

        let mut context = Context::from_waker(&waker);
        file.register(&mut context, interest.event.events);
    }

    // for add/modify
    fn check_and_register_waker(&self, interest: &Arc<EpollInterest>) {
        let Some(file) = interest.key.get_file() else {
            return;
        };

        if !interest.is_enabled() {
            return;
        }

        let waker = Waker::from(Arc::new(InterestWaker {
            epoll: Arc::downgrade(&self.inner),
            interest: Arc::downgrade(interest),
        }));

        let current = file.poll() & interest.event.events;

        if !current.is_empty() {
            waker.wake_by_ref();
        } else {
            let mut context = Context::from_waker(&waker);
            file.register(&mut context, interest.event.events);

            let current = file.poll() & interest.event.events;
            if !current.is_empty() {
                waker.wake_by_ref();
            }
        }
    }

    pub fn add(&self, fd: i32, event: EpollEvent, flags: EpollFlags) -> AxResult<()> {
        let key = EntryKey::new(fd)?;
        let interest = Arc::new(EpollInterest::new(key.clone(), event, flags));
        let mut guard = self.inner.interests.lock();
        if guard.contains_key(&key) {
            return Err(AxError::AlreadyExists);
        }
        guard.insert(key.clone(), Arc::clone(&interest));
        drop(guard);
        trace!("Epoll add fd: {} interest {:?} ", fd, interest.event.events);
        self.check_and_register_waker(&interest);
        Ok(())
    }

    pub fn modify(&self, fd: i32, event: EpollEvent, flags: EpollFlags) -> AxResult<()> {
        let key = EntryKey::new(fd)?;
        let interest = Arc::new(EpollInterest::new(key.clone(), event, flags));

        let mut guard = self.inner.interests.lock();
        let old = guard.get_mut(&key).ok_or(AxError::NotFound)?;

        // update new interest if old already in ready queue
        if old.is_in_queue() {
            interest.in_ready_queue.store(true, Ordering::Release);
        }
        *old = Arc::clone(&interest);
        drop(guard);
        trace!(
            "Epoll: modify fd={}, events={:?}",
            fd, interest.event.events
        );
        // reset waker
        self.check_and_register_waker(&interest);
        Ok(())
    }

    pub fn delete(&self, fd: i32) -> AxResult<()> {
        let key = EntryKey::new(fd)?;
        self.inner
            .interests
            .lock()
            .remove(&key)
            .ok_or(AxError::NotFound)?;
        trace!("Epoll: delete fd={fd}");
        Ok(())
    }

    pub fn poll_events(&self, out: &mut [epoll_event]) -> AxResult<usize> {
        trace!("Epoll: poll_events called, out.len()={}", out.len());
        let mut count = 0;
        loop {
            let weak_interest = {
                let mut queue = self.inner.ready_queue.lock();
                queue.pop_front()
            };

            let Some(weak_interest) = weak_interest else {
                break;
            };

            if count >= out.len() {
                self.inner.ready_queue.lock().push_front(weak_interest);
                break;
            }

            let Some(interest) = weak_interest.upgrade() else {
                continue; // interest already removed
            };

            let Some(file) = interest.key.get_file() else {
                // file already closed remove interests
                self.inner.interests.lock().remove(&interest.key);
                interest.mark_not_in_queue();
                continue;
            };

            trace!(
                "Epoll: consuming ready interest for fd={}, events={:?}",
                interest.key.fd, interest.event.events
            );

            match interest.consume(file.as_ref()) {
                ConsumeResult::EventAndKeep(event) => {
                    out[count] = epoll_event {
                        events: event.events.bits(),
                        data: event.user_data,
                    };
                    count += 1;
                    self.inner
                        .ready_queue
                        .lock()
                        .push_back(Arc::downgrade(&interest));
                }
                ConsumeResult::EventAndRemove(event) => {
                    out[count] = epoll_event {
                        events: event.events.bits(),
                        data: event.user_data,
                    };
                    count += 1;
                    interest.mark_not_in_queue();
                    self.register_waker_only(&interest);
                }
                ConsumeResult::NoEvent => {
                    interest.mark_not_in_queue();
                    self.register_waker_only(&interest);
                }
            }
        }

        if count == 0 {
            Err(AxError::WouldBlock)
        } else {
            Ok(count)
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
        if self.inner.ready_queue.lock().is_empty() {
            IoEvents::empty()
        } else {
            IoEvents::IN
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.inner.poll_ready.register(context.waker());
        }
    }
}
