// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2025 KylinSoft Co., Ltd. <https://www.kylinos.cn/>
// Copyright (C) 2025 Azure-stars <Azure_stars@126.com>
// Copyright (C) 2025 Yuekai Jia <equation618@gmail.com>
// See LICENSES for license details.
//
// This file has been modified by KylinSoft on 2025.

use alloc::{
    collections::vec_deque::VecDeque,
    sync::{Arc, Weak},
    task::Wake,
    vec::Vec,
};
use core::{
    hash::{Hash, Hasher},
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicUsize, Ordering},
    task::Waker,
};

use ax_lazyinit::OnceLock;
use axpoll::{
    ExclusiveConsumer, ExclusiveRegistrationSink, IoEvents, PollRegistrar, SharedObserver,
    SharedRegistrationSink,
};
use axpoll_set::PollSet;
use bitflags::bitflags;
use hashbrown::HashMap;
use linux_raw_sys::general::{EPOLLET, EPOLLEXCLUSIVE, EPOLLONESHOT, epoll_event};

#[cfg(all(test, axtest))]
use super::epoll_axtest::epoll_add_test_barrier;
use super::epoll_topology::{
    EpollTopology, EpollTopologyLink, commit_nested_link, detach_nested_link, lock_epoll_topology,
    prepare_nested_link, reserve_nested_link,
};
use crate::{
    StarryError, StarryResult,
    file::{FileLike, get_file_like, signalfd::Signalfd},
    sync::IrqMutex,
    task::{ProcessData, current_user_task, future::IrqNotify},
};
#[cfg(not(all(test, not(axtest))))]
use crate::sync::Mutex;

static EPOLL_NOTIFY: IrqNotify = IrqNotify::new();
static EPOLL_NOTIFY_QUEUE: IrqMutex<()> = IrqMutex::new(());
static EPOLL_NOTIFY_HEAD: AtomicPtr<EpollInner> = AtomicPtr::new(ptr::null_mut());
static EPOLL_NOTIFY_STARTED: OnceLock<()> = OnceLock::new();

pub(crate) fn start_epoll_notify_worker() {
    EPOLL_NOTIFY_STARTED.call_once(|| {
        crate::task::kernel_thread_builder("epoll-notify".into())
            .spawn(|| loop {
                EPOLL_NOTIFY.wait();
                let mut current = {
                    let _queue = EPOLL_NOTIFY_QUEUE.lock();
                    EPOLL_NOTIFY_HEAD.swap(ptr::null_mut(), Ordering::AcqRel)
                };
                while !current.is_null() {
                    let epoll = unsafe {
                        // SAFETY: queue insertion transfers exactly one strong
                        // reference through Arc::into_raw. The queue lock gives
                        // this sole consumer exclusive ownership of the detached
                        // list, so each node is reconstructed exactly once.
                        Arc::from_raw(current)
                    };
                    let next = epoll.notify_next.swap(ptr::null_mut(), Ordering::Acquire);
                    epoll.notify_queued.store(false, Ordering::SeqCst);
                    epoll.flush_ready_waiters();
                    current = next;
                }
            })
            .expect("failed to spawn epoll notification worker");
    });
}

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
    fn new(fd: i32) -> StarryResult<Self> {
        let file = get_file_like(fd)?;
        if !file.supports_epoll() {
            return Err(StarryError::OperationNotPermitted);
        }
        Ok(Self {
            fd,
            file: Arc::downgrade(&file),
        })
    }

    #[inline]
    fn get_file(&self) -> Option<Arc<dyn FileLike>> {
        self.file.upgrade()
    }

    #[cfg(test)]
    fn for_test(fd: i32, file: &Arc<dyn FileLike>) -> Self {
        Self {
            fd,
            file: Arc::downgrade(file),
        }
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
    nested_link: Option<EpollTopologyLink>,
    // Linux keeps inherited signalfd descriptors readable after fork, but an
    // inherited epoll interest must not observe signals directed to the child.
    // A weak owner preserves same-process waiter refreshes without extending
    // the originating process lifetime.
    signalfd_registration_owner: Option<Weak<ProcessData>>,
    mode: IrqMutex<TriggerMode>,
    exclusive: bool,
    in_ready_queue: AtomicBool,
    owner_repoll_pending: AtomicBool,
    #[cfg(not(all(test, not(axtest))))]
    registration_refresh: Mutex<()>,
    #[cfg(all(test, not(axtest)))]
    registration_refresh: std::sync::Mutex<()>,
    registration: IrqMutex<Option<InterestRegistration>>,
}

enum InterestRegistration {
    Shared(PollRegistrar<SharedObserver>),
    Exclusive(PollRegistrar<ExclusiveConsumer>),
}

impl EpollInterest {
    fn new(
        key: EntryKey,
        event: EpollEvent,
        flags: EpollFlags,
        nested_link: Option<EpollTopologyLink>,
    ) -> Self {
        Self {
            signalfd_registration_owner: key
                .get_file()
                .filter(|file| file.is::<Signalfd>())
                .map(|_| Arc::downgrade(&current_user_task().as_thread().proc_data)),
            key,
            event,
            nested_link,
            mode: IrqMutex::new(TriggerMode::from_flags(flags)),
            exclusive: flags.contains(EpollFlags::EXCLUSIVE),
            in_ready_queue: AtomicBool::new(false),
            owner_repoll_pending: AtomicBool::new(false),
            #[cfg(not(all(test, not(axtest))))]
            registration_refresh: Mutex::new(()),
            #[cfg(all(test, not(axtest)))]
            registration_refresh: std::sync::Mutex::new(()),
            registration: IrqMutex::new(None),
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
    fn is_edge_triggered(&self) -> bool {
        matches!(*self.mode.lock(), TriggerMode::Edge)
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

    fn can_refresh_waker_from_current_process(&self) -> bool {
        self.signalfd_registration_owner
            .as_ref()
            .is_none_or(|owner| {
                owner.upgrade().is_some_and(|owner| {
                    Arc::ptr_eq(&owner, &current_user_task().as_thread().proc_data)
                })
            })
    }

    fn request_owner_repoll(&self) -> bool {
        self.owner_repoll_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn requires_owner_repoll(&self) -> bool {
        self.is_edge_triggered() && self.signalfd_registration_owner.is_some()
    }

    fn take_owner_repoll_request(&self) -> bool {
        self.can_refresh_waker_from_current_process()
            && self.owner_repoll_pending.swap(false, Ordering::AcqRel)
    }

    fn replace_registration(&self, registration: Option<InterestRegistration>) {
        let previous = core::mem::replace(&mut *self.registration.lock(), registration);
        if let Some(mut previous) = previous {
            previous.clear();
        }
    }
}

impl InterestRegistration {
    fn clear(&mut self) {
        match self {
            Self::Shared(registrar) => registrar.clear(),
            Self::Exclusive(registrar) => registrar.clear(),
        }
    }
}

const REGISTRATION_WAKE_REGISTERING: u8 = 0;
const REGISTRATION_WAKE_PENDING: u8 = 1;
const REGISTRATION_WAKE_REGISTERED: u8 = 2;

struct RegistrationWakeState(AtomicU8);

impl RegistrationWakeState {
    fn new() -> Self {
        Self(AtomicU8::new(REGISTRATION_WAKE_REGISTERING))
    }

    fn request_publish(&self) -> bool {
        let mut state = self.0.load(Ordering::Acquire);
        loop {
            match state {
                REGISTRATION_WAKE_REGISTERING => match self.0.compare_exchange(
                    REGISTRATION_WAKE_REGISTERING,
                    REGISTRATION_WAKE_PENDING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return false,
                    Err(observed) => state = observed,
                },
                REGISTRATION_WAKE_PENDING => return false,
                REGISTRATION_WAKE_REGISTERED => return true,
                _ => unreachable!("invalid epoll registration wake state"),
            }
        }
    }

    fn finish_register(&self) -> bool {
        let previous = self
            .0
            .swap(REGISTRATION_WAKE_REGISTERED, Ordering::AcqRel);
        debug_assert_ne!(previous, REGISTRATION_WAKE_REGISTERED);
        previous == REGISTRATION_WAKE_PENDING
    }
}

struct InterestWaker {
    epoll: Weak<EpollInner>,
    interest: Weak<EpollInterest>,
    registration_wake: RegistrationWakeState,
}

impl Wake for InterestWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // The single state modification order hands each wake to exactly one
        // publisher: registration completion or this callback.
        if self.registration_wake.request_publish() {
            self.publish();
        }
    }
}

impl InterestWaker {
    fn new(epoll: &Arc<EpollInner>, interest: &Arc<EpollInterest>) -> Arc<Self> {
        Arc::new(Self {
            epoll: Arc::downgrade(epoll),
            interest: Arc::downgrade(interest),
            registration_wake: RegistrationWakeState::new(),
        })
    }

    fn finish_register(&self, ready: bool) {
        let had_deferred_wake = self.registration_wake.finish_register();
        if ready || had_deferred_wake {
            self.publish();
        }
    }

    fn publish(&self) {
        let Some(epoll) = self.epoll.upgrade() else {
            return;
        };
        let Some(interest) = self.interest.upgrade() else {
            return;
        };

        // signalfd readiness includes the calling thread's pending signals, so
        // even a callback running in the same process cannot safely poll or
        // re-register on behalf of the epoll waiter. A child after fork is an
        // additional case where doing so would steal the parent's registration.
        // Wake the original waiter and let it refresh exactly once in context.
        if interest.requires_owner_repoll() {
            epoll.request_owner_repoll(&interest);
        } else {
            epoll.publish_ready_interest(&interest);
        }
    }
}

pub(super) struct EpollInner {
    interests: IrqMutex<HashMap<EntryKey, Arc<EpollInterest>>>,
    pub(super) topology: EpollTopology,
    ready_queue: IrqMutex<VecDeque<Weak<EpollInterest>>>,
    overflow_ready: AtomicBool,
    poll_ready: PollSet,
    pending_wakes: AtomicUsize,
    notify_queued: AtomicBool,
    notify_next: AtomicPtr<EpollInner>,
}

impl Default for EpollInner {
    fn default() -> Self {
        Self {
            interests: IrqMutex::new(HashMap::new()),
            topology: EpollTopology::default(),
            ready_queue: IrqMutex::new(VecDeque::new()),
            overflow_ready: AtomicBool::new(false),
            poll_ready: PollSet::new(),
            pending_wakes: AtomicUsize::new(0),
            notify_queued: AtomicBool::new(false),
            notify_next: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

impl EpollInner {
    pub(super) fn has_ready_events(&self) -> bool {
        !self.ready_queue.lock().is_empty() || self.overflow_ready.load(Ordering::Acquire)
    }

    pub(super) unsafe fn register_shared_poll_waiter(&self, sink: &mut dyn SharedRegistrationSink) {
        unsafe { sink.register_shared(&self.poll_ready, IoEvents::IN) };
    }

    pub(super) unsafe fn register_exclusive_poll_waiter(
        &self,
        sink: &mut dyn ExclusiveRegistrationSink,
    ) {
        unsafe { sink.register_exclusive(&self.poll_ready, IoEvents::IN) };
    }

    fn register_waker(
        self: &Arc<Self>,
        interest: &Arc<EpollInterest>,
        recheck: bool,
    ) -> Option<Arc<InterestWaker>> {
        #[cfg(not(all(test, not(axtest))))]
        let _refresh = interest.registration_refresh.lock();
        #[cfg(all(test, not(axtest)))]
        let _refresh = interest
            .registration_refresh
            .lock()
            .expect("epoll registration refresh lock poisoned");
        let Some(file) = interest.key.get_file() else {
            interest.replace_registration(None);
            return None;
        };

        if !interest.is_enabled() {
            interest.replace_registration(None);
            return None;
        }

        // A selected callback from the previous registration remains valid
        // until it runs. Serialize refreshes so the stored ownership lease and
        // the new callback cannot be reordered by concurrent epoll waiters.
        let interest_waker = InterestWaker::new(self, interest);
        let waker = Waker::from(Arc::clone(&interest_waker));
        let events = register_events(interest.event.events);
        let registration = if interest.is_exclusive() {
            let mut registrar = PollRegistrar::<ExclusiveConsumer>::new(&waker);
            unsafe { file.register_exclusive(&mut registrar, events) };
            InterestRegistration::Exclusive(registrar)
        } else {
            let mut registrar = PollRegistrar::<SharedObserver>::new(&waker);
            unsafe { file.register_shared(&mut registrar, events) };
            InterestRegistration::Shared(registrar)
        };
        interest.replace_registration(Some(registration));
        let ready = recheck && !match_ready_events(file.poll(), interest.event.events).is_empty();
        interest_waker.finish_register(ready);
        Some(interest_waker)
    }

    /// Remove an interest while the global topology mutex is held.
    fn remove_interest_locked(&self, key: &EntryKey) -> Option<Arc<EpollInterest>> {
        let interest = self.interests.lock().remove(key)?;
        if let Some(link) = &interest.nested_link {
            detach_nested_link(self, link);
        }
        Some(interest)
    }

    /// Remove a stale snapshot only if it is still the current map entry.
    fn remove_invalid_interest(&self, candidate: &Arc<EpollInterest>) {
        let removed = {
            let _topology = lock_epoll_topology();
            let should_remove = self
                .interests
                .lock()
                .get(&candidate.key)
                .is_some_and(|current| Arc::ptr_eq(current, candidate));
            should_remove
                .then(|| self.remove_interest_locked(&candidate.key))
                .flatten()
        };
        drop(removed);
    }

    fn reserve_ready_capacity(&self, min_capacity: usize) -> StarryResult<()> {
        loop {
            if self.ready_queue.lock().capacity() >= min_capacity {
                return Ok(());
            }

            let mut replacement = VecDeque::new();
            replacement
                .try_reserve(min_capacity)
                .map_err(|_| StarryError::NoMemory)?;

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

    fn enqueue_marked_ready_without_wake(&self, interest: &Arc<EpollInterest>) {
        let queued = {
            let mut queue = self.ready_queue.lock();
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
    }

    fn wake_ready_waiters(&self, published: usize) {
        for _ in 0..published {
            // Each registered epoll waiter is exclusive. Stop once no waiter
            // remains instead of needlessly walking an empty poll set.
            if unsafe { self.poll_ready.wake(IoEvents::IN) } == 0 {
                break;
            }
        }
    }

    fn defer_ready_waiters(self: &Arc<Self>, published: usize) {
        if published == 0 {
            return;
        }

        // Keep the count-before-CAS and clear-before-swap pairs in one order:
        // a producer that observes an already queued node must have its count
        // included in that worker's swap, or another node will be queued.
        self.pending_wakes.fetch_add(published, Ordering::SeqCst);
        #[cfg(not(all(test, not(axtest))))]
        if self
            .notify_queued
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let node = Arc::into_raw(Arc::clone(self)).cast_mut();
            {
                // The queue lock serializes publication with list detachment.
                // The transferred Arc keeps this embedded link alive until the
                // notification worker reconstructs and drops that reference.
                let _queue = EPOLL_NOTIFY_QUEUE.lock();
                let head = EPOLL_NOTIFY_HEAD.load(Ordering::Relaxed);
                self.notify_next.store(head, Ordering::Relaxed);
                EPOLL_NOTIFY_HEAD.store(node, Ordering::Release);
            }
            EPOLL_NOTIFY.notify_irq();
        }
    }

    fn flush_ready_waiters(&self) {
        let published = self.pending_wakes.swap(0, Ordering::SeqCst);
        self.wake_ready_waiters(published);
    }

    fn enqueue_marked_ready(&self, interest: &Arc<EpollInterest>) {
        self.enqueue_marked_ready_without_wake(interest);
        self.wake_ready_waiters(1);
    }

    fn publish_ready_interest(self: &Arc<Self>, interest: &Arc<EpollInterest>) {
        let published = {
            let interests = self.interests.lock();
            let is_current = interests
                .get(&interest.key)
                .is_some_and(|current| Arc::ptr_eq(current, interest));
            if !is_current || !interest.is_enabled() || !interest.try_mark_in_queue() {
                0
            } else {
                self.enqueue_marked_ready_without_wake(interest);
                1
            }
        };
        self.defer_ready_waiters(published);
    }

    fn request_owner_repoll(self: &Arc<Self>, interest: &Arc<EpollInterest>) {
        let should_wake = {
            let interests = self.interests.lock();
            interests
                .get(&interest.key)
                .is_some_and(|current| Arc::ptr_eq(current, interest))
                && interest.is_enabled()
        };
        if should_wake && interest.request_owner_repoll() {
            self.defer_ready_waiters(1);
        }
    }

    fn remove_ready_entries_for(&self, target: &Weak<EpollInterest>) {
        self.ready_queue
            .lock()
            .retain(|entry| entry.strong_count() != 0 && !Weak::ptr_eq(entry, target));
    }

    fn drain_ready_queue(&self) -> StarryResult<VecDeque<Weak<EpollInterest>>> {
        loop {
            let len = self.ready_queue.lock().len();
            let mut txlist = VecDeque::new();
            txlist.try_reserve(len).map_err(|_| StarryError::NoMemory)?;

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

    fn snapshot_interests(&self) -> StarryResult<Vec<Arc<EpollInterest>>> {
        loop {
            let len = self.interests.lock().len();
            let mut snapshot = Vec::new();
            snapshot
                .try_reserve(len)
                .map_err(|_| StarryError::NoMemory)?;

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

    fn enqueue_overflow_ready(&self) -> StarryResult<()> {
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
                    self.remove_invalid_interest(&interest);
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
            // Overflow state is published before waking one exclusive waiter.
            unsafe { self.poll_ready.wake(IoEvents::IN) };
        }
        result
    }
}

#[derive(Default)]
pub struct Epoll {
    pub(super) inner: Arc<EpollInner>,
}

impl Epoll {
    pub fn new() -> Self {
        Self::default()
    }

    // only register waker, not add to ready queue
    fn register_waker_only(&self, interest: &Arc<EpollInterest>) {
        if !interest.can_refresh_waker_from_current_process() {
            return;
        }

        let _ = self.inner.register_waker(interest, false);
    }

    fn register_waker_and_recheck(&self, interest: &Arc<EpollInterest>) {
        if !interest.can_refresh_waker_from_current_process() {
            return;
        }

        let _ = self.inner.register_waker(interest, true);
    }

    /// Registers enabled interests with the thread currently waiting in epoll.
    pub fn register_waiter_wakers(&self) -> StarryResult {
        let interests = self.inner.snapshot_interests()?;
        for interest in &interests {
            if interest.take_owner_repoll_request() {
                // A callback consumed outside owner context cannot safely poll
                // signalfd readiness there. Recheck exactly once in the owner
                // waiter without turning ordinary EPOLLET waits into LT polls.
                if self.inner.register_waker(interest, false).is_some() {
                    self.inner.publish_ready_interest(interest);
                }
            } else {
                self.register_waker_only(interest);
            }
        }
        Ok(())
    }

    // for add/modify
    fn check_and_register_waker(&self, interest: &Arc<EpollInterest>) {
        self.register_waker_and_recheck(interest);
    }

    pub fn add(&self, fd: i32, event: EpollEvent, flags: EpollFlags) -> StarryResult<()> {
        let key = EntryKey::new(fd)?;
        self.add_interest(key, event, flags)
    }

    fn add_interest(
        &self,
        key: EntryKey,
        event: EpollEvent,
        flags: EpollFlags,
    ) -> StarryResult<()> {
        let nested_target = key
            .get_file()
            .and_then(|file| file.downcast_arc::<Epoll>().ok())
            .map(|epoll| Arc::clone(&epoll.inner));

        #[cfg(all(test, axtest))]
        epoll_add_test_barrier();

        // Lock order for topology mutation is global topology mutex, then one
        // node's interests/parents/children spinlock. Poll and registration
        // callbacks run only after the global mutex is released.
        let topology = lock_epoll_topology();
        let target_capacity = {
            let mut interests = self.inner.interests.lock();
            if interests.contains_key(&key) {
                return Err(StarryError::AlreadyExists);
            }
            interests
                .try_reserve(1)
                .map_err(|_| StarryError::NoMemory)?;
            interests.len() + 1
        };

        let nested_link = nested_target
            .as_ref()
            .map(|target| prepare_nested_link(&self.inner, target))
            .transpose()?;

        // Complete all fallible allocations before changing either the
        // interest map or the bidirectional topology.
        self.inner.reserve_ready_capacity(target_capacity)?;
        if let Some(target) = &nested_target {
            reserve_nested_link(&self.inner, target)?;
        }

        let interest = Arc::new(EpollInterest::new(
            key.clone(),
            event,
            flags,
            nested_link.clone(),
        ));
        self.inner
            .interests
            .lock()
            .insert(key.clone(), Arc::clone(&interest));
        if let (Some(link), Some(target)) = (&nested_link, &nested_target) {
            commit_nested_link(&self.inner, target, link);
        }
        drop(topology);

        trace!(
            "Epoll add fd: {} interest {:?} ",
            key.fd, interest.event.events
        );
        self.check_and_register_waker(&interest);
        Ok(())
    }

    #[cfg(all(test, axtest))]
    pub(super) fn add_nested_for_test(&self, fd: i32, target: Arc<Epoll>) -> StarryResult<()> {
        let target: Arc<dyn FileLike> = target;
        self.add_interest(
            EntryKey::for_test(fd, &target),
            EpollEvent {
                events: IoEvents::IN,
                user_data: 0,
            },
            EpollFlags::empty(),
        )
    }

    #[cfg(all(test, not(axtest)))]
    pub(super) fn add_file_for_test(
        &self,
        fd: i32,
        target: Arc<dyn FileLike>,
        user_data: u64,
        flags: EpollFlags,
    ) -> StarryResult<()> {
        self.add_interest(
            EntryKey::for_test(fd, &target),
            EpollEvent {
                events: IoEvents::IN,
                user_data,
            },
            flags,
        )
    }

    #[cfg(all(test, not(axtest)))]
    pub(super) fn flush_ready_waiters_for_test(&self) {
        self.inner.flush_ready_waiters();
    }

    #[cfg(all(test, axtest))]
    pub(super) fn defer_ready_waiters_for_test(&self, published: usize) {
        self.inner.defer_ready_waiters(published);
    }

    pub fn modify(&self, fd: i32, event: EpollEvent, flags: EpollFlags) -> StarryResult<()> {
        let key = EntryKey::new(fd)?;

        let topology = lock_epoll_topology();
        let mut guard = self.inner.interests.lock();
        let old = guard.get_mut(&key).ok_or(StarryError::NotFound)?;
        // Linux forbids modifying an entry that was added as exclusive.
        if old.is_exclusive() {
            return Err(StarryError::InvalidInput);
        }
        let interest = Arc::new(EpollInterest::new(
            key.clone(),
            event,
            flags,
            old.nested_link.clone(),
        ));

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
        let old_interest = core::mem::replace(old, Arc::clone(&interest));
        drop(guard);
        drop(topology);
        if was_in_queue {
            self.inner.remove_ready_entries_for(&old_ready_entry);
            self.inner.enqueue_marked_ready(&interest);
        }
        drop(old_interest);
        trace!(
            "Epoll: modify fd={}, events={:?}",
            fd, interest.event.events
        );
        // reset waker
        self.check_and_register_waker(&interest);
        Ok(())
    }

    pub fn delete(&self, fd: i32) -> StarryResult<()> {
        let key = EntryKey::new(fd)?;
        let topology = lock_epoll_topology();
        let interest = self
            .inner
            .remove_interest_locked(&key)
            .ok_or(StarryError::NotFound)?;
        drop(topology);
        let ready_entry = Arc::downgrade(&interest);
        self.inner.remove_ready_entries_for(&ready_entry);
        interest.mark_not_in_queue();
        trace!("Epoll: delete fd={fd}");
        Ok(())
    }

    pub fn poll_events_with(
        &self,
        max_events: usize,
        mut put_event: impl FnMut(usize, epoll_event) -> StarryResult<()>,
    ) -> StarryResult<usize> {
        trace!("Epoll: poll_events_with called, max_events={max_events}");

        self.inner.enqueue_overflow_ready()?;

        // Splice the entire ready_queue into a local txlist, mirroring
        // Linux's ep_send_events. Visiting each interest at most once per
        // epoll_wait prevents the LT path from re-feeding the same fd back
        // into the loop and filling out[] with duplicates of one ready fd.
        let mut txlist = self.inner.drain_ready_queue()?;
        let mut count = 0;
        let mut level_ready: VecDeque<Weak<EpollInterest>> = VecDeque::new();

        while count < max_events {
            let Some(weak_interest) = txlist.pop_front() else {
                break;
            };

            let Some(interest) = weak_interest.upgrade() else {
                continue; // interest already removed
            };

            let Some(file) = interest.key.get_file() else {
                // file already closed remove interests
                self.inner.remove_invalid_interest(&interest);
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
                        self.inner.enqueue_marked_ready_without_wake(&interest);
                        let mut published = 1;
                        for entry in txlist.into_iter().chain(level_ready) {
                            if let Some(interest) = entry.upgrade()
                                && interest.is_in_queue()
                            {
                                self.inner.enqueue_marked_ready_without_wake(&interest);
                                published += 1;
                            }
                        }
                        self.inner.wake_ready_waiters(published);
                        return if count == 0 { Err(err) } else { Ok(count) };
                    }

                    count += 1;
                    if keep_ready {
                        level_ready.push_back(Arc::downgrade(&interest));
                    } else {
                        interest.mark_not_in_queue();
                        self.register_waker_only(&interest);
                    }
                }
                ConsumeResult::NoEvent => {
                    // Register before rechecking only this interest's event
                    // mask. This closes the consume-to-register lost-wakeup
                    // window without treating an unrelated persistent event
                    // such as EPOLLOUT as a phantom match.
                    interest.mark_not_in_queue();
                    self.register_waker_and_recheck(&interest);
                }
            }
        }

        // Linux puts entries not visited because of maxevents before LT
        // entries returned by this scan. That rotation lets successive
        // epoll_wait callers make progress across the ready list.
        let mut published = 0;
        for entry in txlist.into_iter().chain(level_ready) {
            if let Some(interest) = entry.upgrade()
                && interest.is_in_queue()
            {
                self.inner.enqueue_marked_ready_without_wake(&interest);
                published += 1;
            }
        }
        self.inner.wake_ready_waiters(published);

        if count == 0 {
            Err(StarryError::WouldBlock)
        } else {
            Ok(count)
        }
    }
}

#[cfg(all(test, not(axtest)))]
fn epoll_event_matching_rules_hold_for_test() -> bool {
    use axpoll::IoEvents as E;

    // No overlap between current and interested (and no ALWAYS_POLL bits in
    // current) yields the empty set.
    let no_overlap = match_ready_events(E::OUT, E::IN);
    !no_overlap.contains(E::IN) && !no_overlap.contains(E::OUT)
        // Always-poll bits (ERR/HUP) in current are forwarded regardless of
        // the caller's interest mask.
        && match_ready_events(E::HUP, E::OUT).contains(E::HUP)
        && match_ready_events(E::ERR, E::empty()).contains(E::ERR)
        // HUP alone does not synthesize IN. Linux still forwards HUP even if
        // the caller only subscribed to another readiness class.
        && !match_ready_events(E::HUP, E::OUT).contains(E::IN)
        // A source that explicitly reports both HUP and IN preserves both.
        && {
            let m = match_ready_events(E::HUP | E::IN, E::IN);
            m.contains(E::IN) && m.contains(E::HUP)
        }
        // Interested IN with current IN matches.
        && match_ready_events(E::IN, E::IN).contains(E::IN)
        // register_events merges interested with ALWAYS_POLL.
        && (register_events(E::IN).contains(E::IN) && register_events(E::IN).contains(E::ALWAYS_POLL))
        && (register_events(E::empty()).contains(E::ALWAYS_POLL) && !register_events(E::empty()).contains(E::IN))
        // TriggerMode transitions: Level always notifies; Edge always notifies;
        // OneShot notifies once and then goes silent until restored.
        && matches!(TriggerMode::from_flags(EpollFlags::empty()), TriggerMode::Level)
        && matches!(TriggerMode::from_flags(EpollFlags::EDGE_TRIGGER), TriggerMode::Edge)
        && matches!(
            TriggerMode::from_flags(EpollFlags::ONESHOT),
            TriggerMode::OneShot { fired: false }
        )
        // should_notify: LT always true; Edge always true; OneShot true once.
        && TriggerMode::Level.should_notify().0
        && TriggerMode::Edge.should_notify().0
        && {
            let (first, new) = TriggerMode::OneShot { fired: false }.should_notify();
            let (second, _) = new.should_notify();
            first && !second
        }
        // is_enabled: LT and Edge always enabled; OneShot enabled only before fired.
        && TriggerMode::Level.is_enabled()
        && TriggerMode::Edge.is_enabled()
        && TriggerMode::OneShot { fired: false }.is_enabled()
        && !TriggerMode::OneShot { fired: true }.is_enabled()
}

#[cfg(all(test, not(axtest)))]
fn epoll_hup_does_not_synthesize_readable_for_test() -> bool {
    let matched = match_ready_events(IoEvents::HUP, IoEvents::IN);

    matched.bits() == IoEvents::HUP.bits()
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn registration_wake_state_preserves_both_handoff_orders() {
        let wake_first = super::RegistrationWakeState::new();
        assert!(!wake_first.request_publish());
        assert!(wake_first.finish_register());
        assert!(wake_first.request_publish());

        let register_first = super::RegistrationWakeState::new();
        assert!(!register_first.finish_register());
        assert!(register_first.request_publish());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn epoll_event_matching_rules_hold() {
        assert!(super::epoll_event_matching_rules_hold_for_test());
    }

    #[cfg(all(test, not(axtest)))]
    #[test]
    fn epoll_hup_does_not_synthesize_readable() {
        assert!(super::epoll_hup_does_not_synthesize_readable_for_test());
    }
}
