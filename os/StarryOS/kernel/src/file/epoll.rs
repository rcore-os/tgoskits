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
    vec::Vec,
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
use linux_raw_sys::general::{EPOLLET, EPOLLEXCLUSIVE, EPOLLONESHOT, epoll_event};

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
        const EXCLUSIVE = EPOLLEXCLUSIVE;
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
    let mut matched = (current & interested) | (current & IoEvents::ALWAYS_POLL);
    // When the fd is hung up, also force IN so that epoll callers who only
    // inspect EPOLLIN (a common pattern for pipes/sockets) can detect EOF.
    // This is safe because a hung-up fd is always readable (read() returns 0
    // immediately).  Linux epoll reports EPOLLHUP regardless of interest, but
    // applications that mask on EPOLLIN alone still need to see the event.
    // Calling `poll(2)` directly is unaffected by this epoll-only convention.
    if matched.contains(IoEvents::HUP) {
        matched |= IoEvents::IN;
    }
    matched
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
    exclusive: bool,
    in_ready_queue: AtomicBool,
}

impl EpollInterest {
    fn new(key: EntryKey, event: EpollEvent, flags: EpollFlags) -> Self {
        Self {
            key,
            event,
            mode: SpinNoIrq::new(TriggerMode::from_flags(flags)),
            exclusive: flags.contains(EpollFlags::EXCLUSIVE),
            in_ready_queue: AtomicBool::new(false),
        }
    }

    #[inline]
    fn is_exclusive(&self) -> bool {
        self.exclusive
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
        let matched = match_ready_events(current_events, self.event.events);

        // not ready
        if matched.is_empty() {
            return ConsumeResult::NoEvent;
        }

        let mut mode = self.mode.lock();
        let old_mode = *mode;
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
            epoll.enqueue_marked_ready(&interest);
            trace!(
                "Epoll: fd={} added to ready queue, events={:?} wake up poller",
                interest.key.fd, interest.event.events
            );
        }
    }
}

struct EpollInner {
    interests: SpinNoIrq<HashMap<EntryKey, Arc<EpollInterest>>>,
    ready_queue: SpinNoIrq<VecDeque<Weak<EpollInterest>>>,
    overflow_ready: AtomicBool,
    poll_ready: PollSet,
}

impl Default for EpollInner {
    fn default() -> Self {
        Self {
            interests: SpinNoIrq::new(HashMap::new()),
            ready_queue: SpinNoIrq::new(VecDeque::new()),
            overflow_ready: AtomicBool::new(false),
            poll_ready: PollSet::new(),
        }
    }
}

impl EpollInner {
    fn reserve_ready_capacity(&self, min_capacity: usize) -> AxResult<()> {
        loop {
            if self.ready_queue.lock().capacity() >= min_capacity {
                return Ok(());
            }

            let mut replacement = VecDeque::new();
            replacement
                .try_reserve(min_capacity)
                .map_err(|_| AxError::NoMemory)?;

            let mut queue = self.ready_queue.lock();
            if queue.capacity() >= min_capacity {
                return Ok(());
            }
            if queue.len() > replacement.capacity() {
                continue;
            }
            while let Some(entry) = queue.pop_front() {
                replacement.push_back(entry);
            }
            *queue = replacement;
            return Ok(());
        }
    }

    fn enqueue_marked_ready(&self, interest: &Arc<EpollInterest>) {
        let queued = {
            let mut queue = self.ready_queue.lock();
            if queue.len() == queue.capacity() {
                queue.retain(|entry| entry.upgrade().is_some());
            }
            if queue.len() < queue.capacity() {
                queue.push_back(Arc::downgrade(interest));
                true
            } else {
                false
            }
        };

        if !queued {
            interest.mark_not_in_queue();
            self.overflow_ready.store(true, Ordering::Release);
        }
        // Ready queue or overflow state is published before waking epoll waiters.
        unsafe { self.poll_ready.wake(IoEvents::IN) };
    }

    fn remove_ready_entries_for(&self, target: &Weak<EpollInterest>) {
        self.ready_queue
            .lock()
            .retain(|entry| entry.strong_count() != 0 && !Weak::ptr_eq(entry, target));
    }

    fn drain_ready_queue(&self) -> AxResult<VecDeque<Weak<EpollInterest>>> {
        loop {
            let len = self.ready_queue.lock().len();
            let mut txlist = VecDeque::new();
            txlist.try_reserve(len).map_err(|_| AxError::NoMemory)?;

            let mut queue = self.ready_queue.lock();
            if queue.len() > txlist.capacity() {
                continue;
            }
            while let Some(entry) = queue.pop_front() {
                txlist.push_back(entry);
            }
            return Ok(txlist);
        }
    }

    fn snapshot_interests(&self) -> AxResult<Vec<Arc<EpollInterest>>> {
        loop {
            let len = self.interests.lock().len();
            let mut snapshot = Vec::new();
            snapshot.try_reserve(len).map_err(|_| AxError::NoMemory)?;

            let interests = self.interests.lock();
            if interests.len() > snapshot.capacity() {
                continue;
            }
            for interest in interests.values() {
                snapshot.push(Arc::clone(interest));
            }
            return Ok(snapshot);
        }
    }

    fn enqueue_overflow_ready(&self) -> AxResult<()> {
        if !self.overflow_ready.swap(false, Ordering::AcqRel) {
            return Ok(());
        }

        let result = (|| {
            let interests = self.snapshot_interests()?;
            self.reserve_ready_capacity(interests.len())?;
            for interest in interests {
                if interest.is_in_queue() || !interest.is_enabled() {
                    continue;
                }
                let Some(file) = interest.key.get_file() else {
                    self.interests.lock().remove(&interest.key);
                    continue;
                };
                if !match_ready_events(file.poll(), interest.event.events).is_empty()
                    && interest.try_mark_in_queue()
                {
                    self.enqueue_marked_ready(&interest);
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.overflow_ready.store(true, Ordering::Release);
            // Overflow state is published before waking epoll waiters.
            unsafe { self.poll_ready.wake(IoEvents::IN) };
        }
        result
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
        file.register(&mut context, register_events(interest.event.events));
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
        let target_capacity = {
            let guard = self.inner.interests.lock();
            if guard.contains_key(&key) {
                return Err(AxError::AlreadyExists);
            }
            guard.len() + 1
        };
        self.inner.reserve_ready_capacity(target_capacity)?;

        let target_capacity = {
            let mut guard = self.inner.interests.lock();
            if guard.contains_key(&key) {
                return Err(AxError::AlreadyExists);
            }
            guard.insert(key.clone(), Arc::clone(&interest));
            guard.len()
        };
        if let Err(err) = self.inner.reserve_ready_capacity(target_capacity) {
            self.inner.interests.lock().remove(&key);
            return Err(err);
        }
        trace!("Epoll add fd: {} interest {:?} ", fd, interest.event.events);
        self.check_and_register_waker(&interest);
        Ok(())
    }

    pub fn modify(&self, fd: i32, event: EpollEvent, flags: EpollFlags) -> AxResult<()> {
        let key = EntryKey::new(fd)?;
        let interest = Arc::new(EpollInterest::new(key.clone(), event, flags));

        let mut guard = self.inner.interests.lock();
        let old = guard.get_mut(&key).ok_or(AxError::NotFound)?;
        // Linux forbids modifying an entry that was added as exclusive.
        if old.is_exclusive() {
            return Err(AxError::InvalidInput);
        }

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
        let old_ready_entry = Arc::downgrade(old);
        if was_in_queue {
            interest.in_ready_queue.store(true, Ordering::Release);
        }
        *old = Arc::clone(&interest);
        drop(guard);
        if was_in_queue {
            self.inner.remove_ready_entries_for(&old_ready_entry);
            self.inner.enqueue_marked_ready(&interest);
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
        let interest = self
            .inner
            .interests
            .lock()
            .remove(&key)
            .ok_or(AxError::NotFound)?;
        let ready_entry = Arc::downgrade(&interest);
        self.inner.remove_ready_entries_for(&ready_entry);
        interest.mark_not_in_queue();
        trace!("Epoll: delete fd={fd}");
        Ok(())
    }

    pub fn poll_events_with(
        &self,
        max_events: usize,
        mut put_event: impl FnMut(usize, epoll_event) -> AxResult<()>,
    ) -> AxResult<usize> {
        trace!("Epoll: poll_events_with called, max_events={max_events}");

        self.inner.enqueue_overflow_ready()?;

        // Splice the entire ready_queue into a local txlist, mirroring
        // Linux's ep_send_events. Visiting each interest at most once per
        // epoll_wait prevents the LT path from re-feeding the same fd back
        // into the loop and filling out[] with duplicates of one ready fd.
        let mut txlist = self.inner.drain_ready_queue()?;
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
                        interest.in_ready_queue.store(true, Ordering::Release);
                        self.inner.enqueue_marked_ready(&interest);
                        for entry in txlist.into_iter().chain(keep) {
                            if let Some(interest) = entry.upgrade()
                                && interest.is_in_queue()
                            {
                                self.inner.enqueue_marked_ready(&interest);
                            }
                        }
                        return if count == 0 { Err(err) } else { Ok(count) };
                    }

                    count += 1;
                    if keep_ready {
                        keep.push_back(Arc::downgrade(&interest));
                    } else {
                        interest.mark_not_in_queue();
                        // EPOLLET: install a fresh waker so the next edge
                        // transition fires.  There is a race window between
                        // mark_not_in_queue() above and register_waker_only()
                        // below: the previous InterestWaker may have already
                        // been consumed by the wake that delivered the event
                        // we are returning here, leaving the underlying
                        // PollSet empty.  If new data arrives in that gap,
                        // poll_update.wake() hits the empty PollSet and the
                        // notification is silently dropped — EPOLLET would
                        // then never fire again because the new waker is
                        // installed only after the data already arrived.
                        // Close the window by re-checking the file's poll
                        // state after registering and re-queueing the
                        // interest directly if IN-side data is already
                        // present.  EPOLLOUT is intentionally excluded: it
                        // is normally always ready on writable sockets and
                        // would cause a busy-loop.
                        self.register_waker_only(&interest);
                        let in_mask = interest.event.events
                            & (IoEvents::IN | IoEvents::RDHUP | IoEvents::HUP);
                        if !in_mask.is_empty()
                            && let Some(f) = interest.key.get_file()
                            && !(f.poll() & in_mask).is_empty()
                            && interest.try_mark_in_queue()
                        {
                            self.inner.enqueue_marked_ready(&interest);
                        }
                    }
                }
                ConsumeResult::NoEvent => {
                    // Spurious wakeup: the waker fired but file.poll() did
                    // not match the interest mask (e.g. a shared PollSet
                    // wake on a socket that has only EPOLLOUT ready when
                    // the interest is for EPOLLIN).  Re-arm with a plain
                    // waker registration — using check_and_register_waker
                    // here would immediately re-queue the interest via
                    // waker.wake_by_ref() whenever file.poll() is non-empty,
                    // which a connected TCP socket (always EPOLLOUT-ready)
                    // satisfies on every iteration, producing a tight loop
                    // that fills the ready_queue with phantom events.
                    interest.mark_not_in_queue();
                    self.register_waker_only(&interest);
                }
            }
        }

        if !keep.is_empty() {
            for entry in keep {
                if let Some(interest) = entry.upgrade()
                    && interest.is_in_queue()
                {
                    self.inner.enqueue_marked_ready(&interest);
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
    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[eventpoll]".into()
    }
}

impl Pollable for Epoll {
    fn poll(&self) -> IoEvents {
        if self.inner.ready_queue.lock().is_empty()
            && !self.inner.overflow_ready.load(Ordering::Acquire)
        {
            IoEvents::empty()
        } else {
            IoEvents::IN
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            // Registration happens from epoll wait task context.
            unsafe {
                self.inner
                    .poll_ready
                    .register(context.waker(), IoEvents::IN)
            };
        }
    }
}
