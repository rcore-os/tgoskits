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
    hash::{Hash, Hasher},
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Waker},
};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axpoll::{IoEvents, PollSet, Pollable};
use bitflags::bitflags;
use hashbrown::HashMap;
use linux_raw_sys::general::{EPOLLET, EPOLLONESHOT, epoll_event};

use crate::file::{FileLike, get_file_like};

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
    Event {
        event: EpollEvent,
        old_mode: TriggerMode,
        keep_ready: bool,
    },
    // no event and should remove ready list
    NoEvent,
}

fn match_ready_events(current: IoEvents, interested: IoEvents) -> IoEvents {
    (current & interested) | (current & IoEvents::ALWAYS_POLL)
}

fn register_events(interested: IoEvents) -> IoEvents {
    interested | IoEvents::ALWAYS_POLL
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
    mode: SpinNoIrq<TriggerMode>,
    in_ready_queue: AtomicBool,
    edge_wakeup_pending: AtomicBool,
}

impl EpollInterest {
    fn new(key: EntryKey, event: EpollEvent, flags: EpollFlags) -> Self {
        Self {
            key,
            event,
            mode: SpinNoIrq::new(TriggerMode::from_flags(flags)),
            in_ready_queue: AtomicBool::new(false),
            edge_wakeup_pending: AtomicBool::new(false),
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

    #[inline]
    fn mark_edge_wakeup_pending(&self) {
        self.edge_wakeup_pending.store(true, Ordering::Release);
    }

    #[inline]
    fn clear_edge_wakeup_pending(&self) {
        self.edge_wakeup_pending.store(false, Ordering::Release);
    }

    #[inline]
    fn has_edge_wakeup_pending(&self) -> bool {
        self.edge_wakeup_pending.load(Ordering::Acquire)
    }

    fn consume(&self, file: &dyn FileLike) -> ConsumeResult {
        let current_events = file.poll();
        let matched = match_ready_events(current_events, self.event.events);

        // not ready
        if matched.is_empty() {
            return ConsumeResult::NoEvent;
        }

        let mut mode = self.mode.lock();
        let old_mode = *mode;
        if matches!(*mode, TriggerMode::Edge)
            && !self.edge_wakeup_pending.swap(false, Ordering::AcqRel)
        {
            return ConsumeResult::NoEvent;
        }
        let (should_notify, new_mode) = mode.should_notify();
        trace!(
            "consume fd: {} matches {:?} should notify: {} ",
            self.key.fd, matched, should_notify
        );

        if !should_notify {
            return ConsumeResult::NoEvent;
        }

        *mode = new_mode;

        let event = EpollEvent {
            events: matched,
            user_data: self.event.user_data,
        };

        ConsumeResult::Event {
            event,
            old_mode,
            keep_ready: matches!(*mode, TriggerMode::Level),
        }
    }

    fn restore_mode(&self, mode: TriggerMode) {
        *self.mode.lock() = mode;
    }

    fn restore_edge_wakeup(&self, mode: TriggerMode) {
        if matches!(mode, TriggerMode::Edge) {
            self.mark_edge_wakeup_pending();
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

        interest.mark_edge_wakeup_pending();
        if interest.try_mark_in_queue() {
            // The queue lock must disable IRQs because wakers may be invoked
            // from IRQ wake paths. `VecDeque::push_back` can still allocate
            // when capacity is exhausted; if this path is proven to run in IRQ
            // context, replace the queue with a bounded or deferred design.
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
    interests: SpinNoIrq<HashMap<EntryKey, Arc<EpollInterest>>>,
    ready_queue: SpinNoIrq<VecDeque<Weak<EpollInterest>>>,
    poll_ready: PollSet,
}

impl Default for EpollInner {
    fn default() -> Self {
        Self {
            interests: SpinNoIrq::new(HashMap::new()),
            ready_queue: SpinNoIrq::new(VecDeque::new()),
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
        file.register(&mut context, register_events(interest.event.events));
    }

    fn queue_if_pending_edge(&self, interest: &Arc<EpollInterest>) {
        if interest.has_edge_wakeup_pending() && interest.try_mark_in_queue() {
            self.inner
                .ready_queue
                .lock()
                .push_back(Arc::downgrade(interest));
            self.inner.poll_ready.wake();
        }
    }

    fn queue_probe_if_ready(&self, interest: &Arc<EpollInterest>, file: &dyn FileLike) {
        if !interest.is_enabled() {
            return;
        }

        let current = match_ready_events(file.poll(), interest.event.events);
        if current.is_empty() || !interest.try_mark_in_queue() {
            return;
        }

        self.inner
            .ready_queue
            .lock()
            .push_back(Arc::downgrade(interest));
        self.inner.poll_ready.wake();
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

        let current = match_ready_events(file.poll(), interest.event.events);

        if !current.is_empty() {
            waker.wake_by_ref();
        } else {
            let mut context = Context::from_waker(&waker);
            file.register(&mut context, register_events(interest.event.events));

            let current = match_ready_events(file.poll(), interest.event.events);
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

        // Preserve ready-queue membership across the swap. The ready_queue
        // only holds Weak<EpollInterest> pointing at the old Arc, so
        // dropping that Arc below turns those Weaks into dangling handles
        // that upgrade() can't resolve. poll_events() would then silently
        // skip the stale entry and the fd's pending event would be lost —
        // which is how PostgreSQL's EPOLL_CTL_MOD after the first query
        // ended up never waking the backend for the next client packet.
        // Push a fresh Weak for the replacement interest so poll_events()
        // still finds something to consume.
        let was_in_queue = old.is_in_queue();
        if was_in_queue {
            interest.in_ready_queue.store(true, Ordering::Release);
        }
        *old = Arc::clone(&interest);
        drop(guard);
        if was_in_queue {
            self.inner
                .ready_queue
                .lock()
                .push_back(Arc::downgrade(&interest));
            self.inner.poll_ready.wake();
        }
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

    pub fn poll_events_with(
        &self,
        max_events: usize,
        mut put_event: impl FnMut(usize, epoll_event) -> AxResult<()>,
    ) -> AxResult<usize> {
        trace!("Epoll: poll_events_with called, max_events={max_events}");

        // Splice the entire ready_queue into a local txlist, mirroring
        // Linux's ep_send_events. Visiting each interest at most once per
        // epoll_wait prevents the LT path from re-feeding the same fd back
        // into the loop and filling out[] with duplicates of one ready fd.
        let mut txlist = core::mem::take(&mut *self.inner.ready_queue.lock());
        let mut count = 0;
        let mut keep: VecDeque<Weak<EpollInterest>> = VecDeque::new();

        while let Some(weak_interest) = txlist.pop_front() {
            if count >= max_events {
                keep.push_back(weak_interest);
                continue;
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
                ConsumeResult::Event {
                    event,
                    old_mode,
                    keep_ready,
                } => {
                    let event = epoll_event {
                        events: event.events.bits(),
                        data: event.user_data,
                    };

                    if let Err(err) = put_event(count, event) {
                        interest.restore_mode(old_mode);
                        interest.restore_edge_wakeup(old_mode);
                        interest.in_ready_queue.store(true, Ordering::Release);
                        let mut queue = self.inner.ready_queue.lock();
                        queue.push_back(Arc::downgrade(&interest));
                        queue.extend(txlist);
                        queue.extend(keep);
                        drop(queue);
                        self.inner.poll_ready.wake();
                        return if count == 0 { Err(err) } else { Ok(count) };
                    }

                    count += 1;
                    if keep_ready {
                        keep.push_back(Arc::downgrade(&interest));
                    } else {
                        // EPOLLET consumes the current producer wake. Re-arm
                        // before opening the queue slot again; if a producer
                        // wake raced while the old slot was still marked
                        // queued, edge_wakeup_pending preserves it and we
                        // enqueue one replacement entry here.
                        self.register_waker_only(&interest);
                        interest.mark_not_in_queue();
                        self.queue_if_pending_edge(&interest);
                        self.queue_probe_if_ready(&interest, file.as_ref());
                    }
                }
                ConsumeResult::NoEvent => {
                    // Spurious or stale wake: clear the consumed wake bit,
                    // take it out of the queue, and keep a plain waker
                    // installed for a future edge.
                    interest.clear_edge_wakeup_pending();
                    self.register_waker_only(&interest);
                    interest.mark_not_in_queue();
                }
            }
        }

        if !keep.is_empty() {
            let mut queue = self.inner.ready_queue.lock();
            for entry in keep {
                queue.push_back(entry);
            }
            drop(queue);
            self.inner.poll_ready.wake();
        }

        if count == 0 {
            Err(AxError::WouldBlock)
        } else {
            Ok(count)
        }
    }
}

impl FileLike for Epoll {
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[eventpoll]".into()
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
