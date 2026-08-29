use alloc::{borrow::Cow, boxed::Box, collections::VecDeque, format, sync::Arc};
use core::{
    mem,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::Waker,
};

use ax_memory_addr::PAGE_SIZE_4K;
use ax_std::os::arceos::task::{
    WaitQueueRegistration, WaitQueueWakeOutcome, WaitQueueWakeToken, wait_until_registered,
    wake_waker_sync,
};
use axpoll::{IoEvents, PollRegistration, PollSource, Pollable, RegistrationMode};
use linux_raw_sys::{
    general::{O_RDONLY, O_WRONLY, S_IFIFO},
    ioctl::FIONREAD,
};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer},
};
use starry_signal::{SignalInfo, Signo};

use super::{FileLike, Kstat};
use crate::{
    StarryError, StarryResult,
    file::{IoDst, IoSrc},
    mm::VmMutPtr,
    sync::{PiMutex, SpinLock},
    task::{current_user_task, send_signal_to_process},
};

const RING_BUFFER_INIT_SIZE: usize = 65536; // 64 KiB

const RING_BUFFER_MAX_SIZE: usize = 1024 * 1024; // 1 MiB

const PIPE_BUF: usize = PAGE_SIZE_4K;

#[cfg(feature = "qperf-metrics")]
static PIPE_READ_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_READ_WAITS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_READ_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WRITE_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WRITE_WAITS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WRITE_BYTES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WRITE_READER_WAKES_BEFORE_WAIT: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WRITE_READER_WAKES_FINAL: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WRITE_WRITER_WAKES_FINAL: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WAIT_REGISTRATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WAIT_REGISTRATION_RACES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WAKE_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WAKE_SHARED_MATCHES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WAKE_NO_EXCLUSIVE_MATCH: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WAKE_DIRECT_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WAKE_DIRECT_DELIVERED: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WAKE_DIRECT_RETRY: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WAKE_DIRECT_STALE: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static PIPE_WAKE_POLL_DELIVERED: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "qperf-metrics")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PipeQperfMetricsSnapshot {
    pub(crate) read_calls: u64,
    pub(crate) read_waits: u64,
    pub(crate) read_bytes: u64,
    pub(crate) write_calls: u64,
    pub(crate) write_waits: u64,
    pub(crate) write_bytes: u64,
    pub(crate) write_reader_wakes_before_wait: u64,
    pub(crate) write_reader_wakes_final: u64,
    pub(crate) write_writer_wakes_final: u64,
    pub(crate) wait_registrations: u64,
    pub(crate) wait_registration_races: u64,
    pub(crate) wake_calls: u64,
    pub(crate) wake_shared_matches: u64,
    pub(crate) wake_no_exclusive_match: u64,
    pub(crate) wake_direct_attempts: u64,
    pub(crate) wake_direct_delivered: u64,
    pub(crate) wake_direct_retry: u64,
    pub(crate) wake_direct_stale: u64,
    pub(crate) wake_poll_delivered: u64,
}

#[cfg(feature = "qperf-metrics")]
pub(crate) fn qperf_metrics_snapshot() -> PipeQperfMetricsSnapshot {
    PipeQperfMetricsSnapshot {
        read_calls: PIPE_READ_CALLS.load(Ordering::Relaxed),
        read_waits: PIPE_READ_WAITS.load(Ordering::Relaxed),
        read_bytes: PIPE_READ_BYTES.load(Ordering::Relaxed),
        write_calls: PIPE_WRITE_CALLS.load(Ordering::Relaxed),
        write_waits: PIPE_WRITE_WAITS.load(Ordering::Relaxed),
        write_bytes: PIPE_WRITE_BYTES.load(Ordering::Relaxed),
        write_reader_wakes_before_wait: PIPE_WRITE_READER_WAKES_BEFORE_WAIT.load(Ordering::Relaxed),
        write_reader_wakes_final: PIPE_WRITE_READER_WAKES_FINAL.load(Ordering::Relaxed),
        write_writer_wakes_final: PIPE_WRITE_WRITER_WAKES_FINAL.load(Ordering::Relaxed),
        wait_registrations: PIPE_WAIT_REGISTRATIONS.load(Ordering::Relaxed),
        wait_registration_races: PIPE_WAIT_REGISTRATION_RACES.load(Ordering::Relaxed),
        wake_calls: PIPE_WAKE_CALLS.load(Ordering::Relaxed),
        wake_shared_matches: PIPE_WAKE_SHARED_MATCHES.load(Ordering::Relaxed),
        wake_no_exclusive_match: PIPE_WAKE_NO_EXCLUSIVE_MATCH.load(Ordering::Relaxed),
        wake_direct_attempts: PIPE_WAKE_DIRECT_ATTEMPTS.load(Ordering::Relaxed),
        wake_direct_delivered: PIPE_WAKE_DIRECT_DELIVERED.load(Ordering::Relaxed),
        wake_direct_retry: PIPE_WAKE_DIRECT_RETRY.load(Ordering::Relaxed),
        wake_direct_stale: PIPE_WAKE_DIRECT_STALE.load(Ordering::Relaxed),
        wake_poll_delivered: PIPE_WAKE_POLL_DELIVERED.load(Ordering::Relaxed),
    }
}

fn wake_pipe_waiter_sync(waiters: &PipeWaitSet, ready: IoEvents) {
    waiters.wake(ready, true);
}

fn wake_pipe_waiter(waiters: &PipeWaitSet, ready: IoEvents) {
    waiters.wake(ready, false);
}

fn wake_pipe_waiters_all(waiters: &PipeWaitSet, ready: IoEvents) {
    waiters.wake_all(ready);
}

struct PipeWaitSet {
    state: Arc<SpinLock<PipeWaitState>>,
}

struct PipeWaitState {
    waiters: VecDeque<PipeWaiter>,
    next_id: u64,
    notification_generation: u64,
    closed: bool,
}

struct PipeWaiter {
    id: u64,
    target: PipeWaitTarget,
}

enum PipeWaitTarget {
    Direct(WaitQueueWakeToken),
    Poll {
        waker: Waker,
        interests: IoEvents,
        mode: RegistrationMode,
        notified: Arc<AtomicBool>,
    },
}

struct PipeWaitRegistration {
    state: Arc<SpinLock<PipeWaitState>>,
    id: u64,
    notified: Option<Arc<AtomicBool>>,
}

struct PipeWakeSelection {
    shared: VecDeque<PipeWaiter>,
    exclusive: Option<PipeWaiter>,
}

impl PipeWaitSet {
    fn new() -> Self {
        Self {
            state: Arc::new(SpinLock::new(PipeWaitState {
                waiters: VecDeque::new(),
                next_id: 0,
                notification_generation: 0,
                closed: false,
            })),
        }
    }

    fn wait_until(&self, condition: impl Fn() -> bool) -> bool {
        let _selected = wait_until_registered(
            condition,
            || self.state.lock().notification_generation,
            |token, observed_notification| {
                #[cfg(feature = "qperf-metrics")]
                PIPE_WAIT_REGISTRATIONS.fetch_add(1, Ordering::Relaxed);
                let mut state = self.state.lock();
                if state.closed || state.notification_generation != observed_notification {
                    #[cfg(feature = "qperf-metrics")]
                    PIPE_WAIT_REGISTRATION_RACES.fetch_add(1, Ordering::Relaxed);
                    WaitQueueRegistration::Retry(None)
                } else {
                    let registration =
                        self.register_target_locked(&mut state, PipeWaitTarget::Direct(token));
                    WaitQueueRegistration::Armed(Some(registration))
                }
            },
        );
        // Linux sets wake_next_reader/writer after every return from
        // wait_event_interruptible_exclusive(). This includes a condition that
        // becomes true after the caller observed WouldBlock but before the
        // waiter consumes a wake quota.
        true
    }

    fn register_target(&self, target: PipeWaitTarget) -> Option<PipeWaitRegistration> {
        let mut state = self.state.lock();
        if state.closed {
            return None;
        }
        Some(self.register_target_locked(&mut state, target))
    }

    fn register_target_locked(
        &self,
        state: &mut PipeWaitState,
        target: PipeWaitTarget,
    ) -> PipeWaitRegistration {
        let id = state.next_id;
        state.next_id = state
            .next_id
            .checked_add(1)
            .expect("pipe waiter registration ID space exhausted");
        let notified = target.notified();
        let waiter = PipeWaiter { id, target };
        if waiter.target.mode() == RegistrationMode::Shared {
            // Linux add_wait_queue() inserts non-exclusive poll entries at the
            // head. Direct and EPOLLEXCLUSIVE waiters stay FIFO at the tail.
            state.waiters.push_front(waiter);
        } else {
            state.waiters.push_back(waiter);
        }
        PipeWaitRegistration {
            state: Arc::clone(&self.state),
            id,
            notified,
        }
    }

    fn wake(&self, ready: IoEvents, sync: bool) {
        #[cfg(feature = "qperf-metrics")]
        PIPE_WAKE_CALLS.fetch_add(1, Ordering::Relaxed);
        let (boundary, selected) = {
            let mut state = self.state.lock();
            state.notification_generation = state
                .notification_generation
                .checked_add(1)
                .expect("pipe notification generation exhausted");
            let boundary = state.next_id;
            let mut selected = PipeWakeSelection {
                shared: VecDeque::new(),
                exclusive: None,
            };
            let mut index = 0;
            while index < state.waiters.len() {
                let waiter = &state.waiters[index];
                if waiter.id >= boundary || !waiter.target.matches(ready) {
                    index += 1;
                    continue;
                }
                let mode = waiter.target.mode();
                if mode == RegistrationMode::Exclusive && selected.exclusive.is_some() {
                    index += 1;
                    continue;
                }
                let waiter = state
                    .waiters
                    .remove(index)
                    .expect("located pipe waiter must remain present under its lock");
                waiter.target.mark_selected();
                match mode {
                    RegistrationMode::Shared => selected.shared.push_back(waiter),
                    RegistrationMode::Exclusive => selected.exclusive = Some(waiter),
                }
            }
            (boundary, selected)
        };
        #[cfg(feature = "qperf-metrics")]
        PIPE_WAKE_SHARED_MATCHES.fetch_add(selected.shared.len() as u64, Ordering::Relaxed);

        // Readiness was published before selection. Invoke callbacks after
        // dropping the waitqueue lock so re-entrant poll paths cannot deadlock.
        for waiter in selected.shared {
            waiter.target.wake_poll(sync);
        }

        let mut selected_exclusive = selected.exclusive;
        let mut after_id = None;
        loop {
            let waiter = selected_exclusive
                .take()
                .or_else(|| self.take_next_exclusive(ready, boundary, after_id));
            let Some(waiter) = waiter else {
                #[cfg(feature = "qperf-metrics")]
                PIPE_WAKE_NO_EXCLUSIVE_MATCH.fetch_add(1, Ordering::Relaxed);
                return;
            };
            after_id = Some(waiter.id);
            match &waiter.target {
                PipeWaitTarget::Direct(token) => {
                    #[cfg(feature = "qperf-metrics")]
                    PIPE_WAKE_DIRECT_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
                    let outcome = if sync {
                        token.notify_sync()
                    } else {
                        token.notify()
                    };
                    match outcome {
                        WaitQueueWakeOutcome::Delivered => {
                            #[cfg(feature = "qperf-metrics")]
                            PIPE_WAKE_DIRECT_DELIVERED.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                        WaitQueueWakeOutcome::Retry => {
                            #[cfg(feature = "qperf-metrics")]
                            PIPE_WAKE_DIRECT_RETRY.fetch_add(1, Ordering::Relaxed);
                            self.reinsert_if_active(waiter);
                        }
                        WaitQueueWakeOutcome::Stale => {
                            #[cfg(feature = "qperf-metrics")]
                            PIPE_WAKE_DIRECT_STALE.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                PipeWaitTarget::Poll { waker, .. } => {
                    #[cfg(feature = "qperf-metrics")]
                    PIPE_WAKE_POLL_DELIVERED.fetch_add(1, Ordering::Relaxed);
                    if sync {
                        wake_waker_sync(waker.clone());
                    } else {
                        waker.wake_by_ref();
                    }
                    return;
                }
            }
        }
    }

    fn take_next_exclusive(
        &self,
        ready: IoEvents,
        boundary: u64,
        after_id: Option<u64>,
    ) -> Option<PipeWaiter> {
        let mut state = self.state.lock();
        let index = state.waiters.iter().position(|waiter| {
            waiter.id < boundary
                && after_id.is_none_or(|after_id| waiter.id > after_id)
                && waiter.target.mode() == RegistrationMode::Exclusive
                && waiter.target.matches(ready)
        })?;
        let waiter = state
            .waiters
            .remove(index)
            .expect("located pipe waiter must remain present under its lock");
        waiter.target.mark_selected();
        Some(waiter)
    }

    fn reinsert_if_active(&self, waiter: PipeWaiter) {
        let mut state = self.state.lock();
        if state.closed || !waiter.target.is_active() {
            return;
        }
        let index = state
            .waiters
            .iter()
            .position(|queued| {
                queued.target.mode() == RegistrationMode::Exclusive && queued.id > waiter.id
            })
            .unwrap_or(state.waiters.len());
        state.waiters.insert(index, waiter);
    }

    fn wake_all(&self, ready: IoEvents) {
        let waiters = {
            let mut state = self.state.lock();
            state.notification_generation = state
                .notification_generation
                .checked_add(1)
                .expect("pipe notification generation exhausted");
            let boundary = state.next_id;
            let mut selected = VecDeque::new();
            let mut retained = VecDeque::new();
            while let Some(waiter) = state.waiters.pop_front() {
                if waiter.id < boundary && waiter.target.matches(ready) {
                    waiter.target.mark_selected();
                    selected.push_back(waiter);
                } else {
                    retained.push_back(waiter);
                }
            }
            state.waiters = retained;
            selected
        };
        for waiter in waiters {
            match waiter.target {
                PipeWaitTarget::Direct(token) => {
                    let _ = token.notify();
                }
                PipeWaitTarget::Poll { waker, .. } => waker.wake(),
            }
        }
    }
}

impl PipeWaitTarget {
    fn mode(&self) -> RegistrationMode {
        match self {
            Self::Direct(_) => RegistrationMode::Exclusive,
            Self::Poll { mode, .. } => *mode,
        }
    }

    fn matches(&self, ready: IoEvents) -> bool {
        match self {
            Self::Direct(_) => true,
            Self::Poll { interests, .. } => interests.intersects(ready),
        }
    }

    fn notified(&self) -> Option<Arc<AtomicBool>> {
        match self {
            Self::Direct(_) => None,
            Self::Poll { notified, .. } => Some(Arc::clone(notified)),
        }
    }

    fn wake_poll(self, sync: bool) {
        let Self::Poll { waker, .. } = self else {
            unreachable!("shared pipe waiters are always poll registrations");
        };
        if sync {
            wake_waker_sync(waker);
        } else {
            waker.wake();
        }
    }

    fn mark_selected(&self) {
        if let Self::Poll { notified, .. } = self {
            notified.store(true, Ordering::Release);
        }
    }

    fn is_active(&self) -> bool {
        match self {
            Self::Direct(token) => token.is_active(),
            Self::Poll { .. } => false,
        }
    }
}

impl PollRegistration for PipeWaitRegistration {
    fn was_notified(&self) -> bool {
        self.notified
            .as_ref()
            .is_some_and(|notified| notified.load(Ordering::Acquire))
    }
}

impl Drop for PipeWaitRegistration {
    fn drop(&mut self) {
        let mut state = self.state.lock();
        if let Some(index) = state.waiters.iter().position(|waiter| waiter.id == self.id) {
            state.waiters.remove(index);
        }
    }
}

impl PollSource for PipeWaitSet {
    unsafe fn register(
        &self,
        waker: &Waker,
        interests: IoEvents,
        mode: RegistrationMode,
    ) -> Option<Box<dyn PollRegistration>> {
        let notified = Arc::new(AtomicBool::new(false));
        self.register_target(PipeWaitTarget::Poll {
            waker: waker.clone(),
            interests,
            mode,
            notified,
        })
        .map(|registration| Box::new(registration) as Box<dyn PollRegistration>)
    }
}

impl Drop for PipeWaitSet {
    fn drop(&mut self) {
        let waiters = {
            let mut state = self.state.lock();
            state.closed = true;
            state.notification_generation = state
                .notification_generation
                .checked_add(1)
                .expect("pipe notification generation exhausted");
            let waiters = core::mem::take(&mut state.waiters);
            for waiter in &waiters {
                waiter.target.mark_selected();
            }
            waiters
        };
        for waiter in waiters {
            match waiter.target {
                PipeWaitTarget::Direct(token) => {
                    let _ = token.notify();
                }
                PipeWaitTarget::Poll { waker, .. } => {
                    waker.wake();
                }
            }
        }
    }
}

struct Shared {
    state: PiMutex<PipeState>,
    // One coherent Linux-style READ_ONCE view of data, capacity, and peers.
    readiness: AtomicU64,
    wait_rx: PipeWaitSet,
    wait_tx: PipeWaitSet,
    poll_usage: AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PipeReadiness(u64);

struct PipeState {
    buffer: HeapRb<u8>,
    buffers: VecDeque<PipeBuffer>,
    readers: usize,
    writers: usize,
}

struct PipeBuffer {
    offset: usize,
    length: usize,
}

impl PipeBuffer {
    fn new(length: usize) -> Self {
        debug_assert!((1..=PIPE_BUF).contains(&length));
        Self { offset: 0, length }
    }

    fn can_merge(&self, bytes: usize) -> bool {
        self.offset
            .checked_add(self.length)
            .and_then(|end| end.checked_add(bytes))
            .is_some_and(|end| end <= PIPE_BUF)
    }

    fn consume(&mut self, bytes: usize) {
        debug_assert!(bytes <= self.length);
        self.offset = self
            .offset
            .checked_add(bytes)
            .expect("pipe buffer offset overflowed its page slot");
        self.length -= bytes;
    }
}

enum PipeWriteWakePhase {
    BeforeWait,
    Final,
}

fn pipe_write_reader_wake_due(
    was_empty: bool,
    poll_usage: bool,
    phase: PipeWriteWakePhase,
) -> bool {
    match phase {
        PipeWriteWakePhase::BeforeWait => was_empty,
        PipeWriteWakePhase::Final => was_empty || poll_usage,
    }
}

impl Shared {
    fn new(state: PipeState) -> Self {
        let readiness = state.readiness();
        Self {
            state: PiMutex::new(state),
            readiness: AtomicU64::new(readiness.0),
            wait_rx: PipeWaitSet::new(),
            wait_tx: PipeWaitSet::new(),
            poll_usage: AtomicBool::new(false),
        }
    }

    fn readiness(&self) -> PipeReadiness {
        // State transitions publish before advancing the wait-set generation.
        // The generation recheck closes the predicate-to-registration window;
        // this Acquire makes the winning publication visible to the predicate.
        PipeReadiness(self.readiness.load(Ordering::Acquire))
    }

    fn update_state<R>(&self, update: impl FnOnce(&mut PipeState) -> R) -> R {
        let mut state = self.state.lock();
        let result = update(&mut state);
        // Every PipeState mutation goes through this boundary. Publish while
        // still holding the state lock, before the caller can notify either
        // wait set, so poll and blocking predicates observe one coherent mask.
        self.readiness.store(state.readiness().0, Ordering::Release);
        result
    }
}

impl PipeReadiness {
    const DATA_AVAILABLE: u64 = 1 << 0;
    const SPACE_AVAILABLE: u64 = 1 << 1;
    const HAS_READERS: u64 = 1 << 2;
    const HAS_WRITERS: u64 = 1 << 3;

    fn read_wait_ready(self) -> bool {
        self.contains(Self::DATA_AVAILABLE) || !self.contains(Self::HAS_WRITERS)
    }

    fn write_wait_ready(self) -> bool {
        self.contains(Self::SPACE_AVAILABLE) || !self.contains(Self::HAS_READERS)
    }

    fn poll_events(self, read_side: bool) -> IoEvents {
        let mut events = IoEvents::empty();
        if read_side {
            events.set(
                IoEvents::IN | IoEvents::RDNORM,
                self.contains(Self::DATA_AVAILABLE),
            );
            events.set(IoEvents::HUP, !self.contains(Self::HAS_WRITERS));
        } else {
            events.set(IoEvents::ERR, !self.contains(Self::HAS_READERS));
            events.set(
                IoEvents::OUT | IoEvents::WRNORM,
                self.contains(Self::SPACE_AVAILABLE),
            );
        }
        events
    }

    fn contains(self, state: u64) -> bool {
        self.0 & state != 0
    }
}

impl PipeState {
    #[cfg(all(test, axtest))]
    fn new(capacity: usize) -> Self {
        Self {
            buffer: HeapRb::new(capacity),
            buffers: VecDeque::new(),
            readers: 1,
            writers: 1,
        }
    }

    fn has_free_buffer(&self) -> bool {
        self.buffers.len() < self.buffer.capacity().get() / PIPE_BUF
    }

    fn can_merge(&self, bytes: usize) -> bool {
        self.buffers
            .back()
            .is_some_and(|buffer| buffer.can_merge(bytes))
    }

    fn readiness(&self) -> PipeReadiness {
        let mut readiness = 0;
        if !self.buffer.is_empty() {
            readiness |= PipeReadiness::DATA_AVAILABLE;
        }
        if self.has_free_buffer() {
            readiness |= PipeReadiness::SPACE_AVAILABLE;
        }
        if self.readers != 0 {
            readiness |= PipeReadiness::HAS_READERS;
        }
        if self.writers != 0 {
            readiness |= PipeReadiness::HAS_WRITERS;
        }
        PipeReadiness(readiness)
    }

    fn copy_from(&mut self, src: &mut IoSrc, limit: usize) -> StarryResult<usize> {
        let (left, right) = self.buffer.vacant_slices_mut();
        let left_limit = left.len().min(limit);
        // `left` covers vacant ring storage and the following `read` initializes
        // exactly the returned prefix before the write index is advanced.
        let left = unsafe { left.assume_init_mut() };
        let mut copied = src.read(&mut left[..left_limit])?;
        if copied == left_limit && copied < limit {
            let right_limit = right.len().min(limit - copied);
            // The same vacant-storage contract applies to the wrapped slice.
            let right = unsafe { right.assume_init_mut() };
            copied += src.read(&mut right[..right_limit])?;
        }
        // Both reads initialized the first `copied` bytes across the two vacant
        // slices, and neither slice aliases occupied ring contents.
        unsafe { self.buffer.advance_write_index(copied) };
        Ok(copied)
    }

    fn merge_from(&mut self, src: &mut IoSrc, bytes: usize) -> StarryResult<usize> {
        debug_assert!(self.can_merge(bytes));
        let copied = self.copy_from(src, bytes)?;
        self.buffers
            .back_mut()
            .expect("merge requires an existing pipe buffer")
            .length += copied;
        Ok(copied)
    }

    fn append_from(&mut self, src: &mut IoSrc) -> StarryResult<usize> {
        debug_assert!(self.has_free_buffer());
        let limit = src.remaining().min(PIPE_BUF);
        let copied = self.copy_from(src, limit)?;
        if copied > 0 {
            self.buffers.push_back(PipeBuffer::new(copied));
        }
        Ok(copied)
    }

    fn consume(&mut self, mut bytes: usize) {
        while bytes > 0 {
            let front = self
                .buffers
                .front_mut()
                .expect("pipe bytes require a pipe buffer");
            let consumed = bytes.min(front.length);
            front.consume(consumed);
            bytes -= consumed;
            if front.length == 0 {
                self.buffers.pop_front();
            }
        }
    }

    #[cfg(all(test, axtest))]
    fn poll_events(&self, read_side: bool) -> IoEvents {
        self.readiness().poll_events(read_side)
    }
}

pub struct Pipe {
    read_side: bool,
    shared: Arc<Shared>,
    non_blocking: AtomicBool,
}

impl Drop for Pipe {
    fn drop(&mut self) {
        if self.read_side {
            let wake_writers = self.shared.update_state(|state| {
                debug_assert!(state.readers > 0);
                state.readers = state.readers.saturating_sub(1);
                state.readers == 0
            });
            if wake_writers {
                // Reader count is published before waking blocked writers.
                wake_pipe_waiters_all(&self.shared.wait_tx, IoEvents::ERR | IoEvents::OUT);
            }
            return;
        }

        let wake_readers = self.shared.update_state(|state| {
            debug_assert!(state.writers > 0);
            state.writers = state.writers.saturating_sub(1);
            state.writers == 0
        });
        if wake_readers {
            // Writer count is published before waking blocked readers.
            wake_pipe_waiters_all(&self.shared.wait_rx, IoEvents::HUP | IoEvents::IN);
        }
    }
}

impl Pipe {
    pub fn new() -> (Pipe, Pipe) {
        let shared = Arc::new(Shared::new(PipeState {
            buffer: HeapRb::new(RING_BUFFER_INIT_SIZE),
            buffers: VecDeque::new(),
            readers: 1,
            writers: 1,
        }));
        let read_end = Pipe {
            read_side: true,
            shared: shared.clone(),
            non_blocking: AtomicBool::new(false),
        };
        let write_end = Pipe {
            read_side: false,
            shared,
            non_blocking: AtomicBool::new(false),
        };
        (read_end, write_end)
    }

    /// Opens another file description for the same pipe endpoint.
    ///
    /// Unlike `dup`, reopening `/proc/self/fd/<n>` creates independent file
    /// status flags while retaining the same underlying pipe buffer. The
    /// endpoint count must therefore be incremented so closing either file
    /// description cannot prematurely report EOF or a broken pipe.
    pub(crate) fn reopen(&self, non_blocking: bool) -> Pipe {
        self.shared.update_state(|state| {
            if self.read_side {
                state.readers += 1;
            } else {
                state.writers += 1;
            }
        });

        Pipe {
            read_side: self.read_side,
            shared: self.shared.clone(),
            non_blocking: AtomicBool::new(non_blocking),
        }
    }

    pub const fn is_read(&self) -> bool {
        self.read_side
    }

    pub const fn is_write(&self) -> bool {
        !self.read_side
    }

    pub fn capacity(&self) -> usize {
        self.shared.state.lock().buffer.capacity().get()
    }

    pub fn resize(&self, new_size: usize) -> StarryResult<()> {
        let new_size = rounded_pipe_size(new_size)?;

        let expanded = self.shared.update_state(|state| -> StarryResult<bool> {
            let old_size = state.buffer.capacity().get();
            if new_size == old_size {
                return Ok(false);
            }
            if new_size / PIPE_BUF < state.buffers.len() {
                return Err(StarryError::ResourceBusy);
            }
            let old_buffer = mem::replace(
                &mut state.buffer,
                HeapRb::try_new(new_size).map_err(|_| StarryError::NoMemory)?,
            );
            let (left, right) = old_buffer.as_slices();
            let copied = state.buffer.push_slice(left) + state.buffer.push_slice(right);
            debug_assert_eq!(copied, left.len() + right.len());
            Ok(new_size > old_size)
        })?;

        if expanded {
            // Newly freed capacity is visible before waking writers.
            wake_pipe_waiter(&self.shared.wait_tx, IoEvents::OUT);
        }
        Ok(())
    }

    fn wake_readers_before_write_wait(&self, was_empty: bool) {
        let wake_readers = pipe_write_reader_wake_due(
            was_empty,
            self.shared.poll_usage.load(Ordering::Acquire),
            PipeWriteWakePhase::BeforeWait,
        );
        if wake_readers {
            #[cfg(feature = "qperf-metrics")]
            PIPE_WRITE_READER_WAKES_BEFORE_WAIT.fetch_add(1, Ordering::Relaxed);
            wake_pipe_waiter_sync(&self.shared.wait_rx, IoEvents::IN);
        }
    }

    fn finish_write_wakes(&self, was_empty: bool, wake_next_writer: bool) {
        let readiness = self.shared.readiness();
        let wake_readers = pipe_write_reader_wake_due(
            was_empty,
            self.shared.poll_usage.load(Ordering::Acquire),
            PipeWriteWakePhase::Final,
        );
        if wake_readers {
            #[cfg(feature = "qperf-metrics")]
            PIPE_WRITE_READER_WAKES_FINAL.fetch_add(1, Ordering::Relaxed);
            wake_pipe_waiter_sync(&self.shared.wait_rx, IoEvents::IN);
        }
        if wake_next_writer && readiness.contains(PipeReadiness::SPACE_AVAILABLE) {
            #[cfg(feature = "qperf-metrics")]
            PIPE_WRITE_WRITER_WAKES_FINAL.fetch_add(1, Ordering::Relaxed);
            wake_pipe_waiter_sync(&self.shared.wait_tx, IoEvents::OUT);
        }
    }

    fn write_with_broken_pipe_handler(
        &self,
        src: &mut IoSrc,
        on_broken_pipe: impl Fn(),
    ) -> StarryResult<usize> {
        if !self.is_write() {
            return Err(StarryError::BadFileDescriptor);
        }
        let size = src.remaining();
        if size == 0 {
            return Ok(0);
        }
        #[cfg(feature = "qperf-metrics")]
        PIPE_WRITE_CALLS.fetch_add(1, Ordering::Relaxed);

        enum WriteStep {
            Closed,
            WouldBlock,
            Wrote(usize),
        }

        let mut total_written = 0;
        let mut merge_pending = true;
        let merge_bytes = size % PIPE_BUF;
        let mut sample_initial_was_empty = true;
        let mut refresh_was_empty_after_wait = false;
        let mut was_empty = false;
        let mut wake_next_writer = false;
        let mut wait_recorded = false;
        let mut task = None;
        loop {
            let step = self
                .shared
                .update_state(|state| -> StarryResult<WriteStep> {
                    if refresh_was_empty_after_wait {
                        was_empty = state.buffer.is_empty();
                        refresh_was_empty_after_wait = false;
                    }
                    if state.readers == 0 {
                        return Ok(WriteStep::Closed);
                    }
                    if sample_initial_was_empty {
                        was_empty = state.buffer.is_empty();
                        sample_initial_was_empty = false;
                    }

                    let mut written = 0;
                    if merge_pending {
                        merge_pending = false;
                        if merge_bytes > 0 && state.can_merge(merge_bytes) {
                            written += state.merge_from(src, merge_bytes)?;
                        }
                    }
                    while src.remaining() > 0 && state.has_free_buffer() {
                        let appended = state.append_from(src)?;
                        written += appended;
                        if appended == 0 {
                            break;
                        }
                    }
                    if written == 0 {
                        Ok(WriteStep::WouldBlock)
                    } else {
                        Ok(WriteStep::Wrote(written))
                    }
                });

            let step = match step {
                Ok(step) => step,
                Err(error) => {
                    self.finish_write_wakes(was_empty, wake_next_writer);
                    return Err(error);
                }
            };
            match step {
                WriteStep::Closed => {
                    on_broken_pipe();
                    self.finish_write_wakes(was_empty, wake_next_writer);
                    if total_written > 0 {
                        return Ok(total_written);
                    }
                    return Err(StarryError::BrokenPipe);
                }
                WriteStep::WouldBlock => {}
                WriteStep::Wrote(written) => {
                    #[cfg(feature = "qperf-metrics")]
                    PIPE_WRITE_BYTES.fetch_add(written as u64, Ordering::Relaxed);
                    total_written += written;
                    if total_written == size || self.nonblocking() {
                        self.finish_write_wakes(was_empty, wake_next_writer);
                        return Ok(total_written);
                    }
                }
            }

            if self.nonblocking() {
                self.finish_write_wakes(was_empty, wake_next_writer);
                return Err(StarryError::WouldBlock);
            }
            if !wait_recorded {
                #[cfg(feature = "qperf-metrics")]
                PIPE_WRITE_WAITS.fetch_add(1, Ordering::Relaxed);
                wait_recorded = true;
            }

            let task = task.get_or_insert_with(current_user_task);
            if task.take_interrupt() {
                // Linux returns committed bytes instead of EINTR once a pipe
                // write has made progress, so SA_RESTART cannot replay them.
                self.finish_write_wakes(was_empty, wake_next_writer);
                return if total_written > 0 {
                    Ok(total_written)
                } else {
                    Err(StarryError::Interrupted)
                };
            }
            self.wake_readers_before_write_wait(was_empty);
            self.shared
                .wait_tx
                .wait_until(|| self.shared.readiness().write_wait_ready() || task.interrupted());
            refresh_was_empty_after_wait = true;
            wake_next_writer = true;
        }
    }

    #[cfg(all(test, axtest))]
    fn duplicate_read_end_for_test(&self) -> Pipe {
        assert!(self.is_read());
        self.shared.update_state(|state| state.readers += 1);
        Pipe {
            read_side: true,
            shared: self.shared.clone(),
            non_blocking: AtomicBool::new(self.nonblocking()),
        }
    }

    #[cfg(all(test, axtest))]
    fn duplicate_write_end_for_test(&self) -> Pipe {
        assert!(self.is_write());
        self.shared.update_state(|state| state.writers += 1);
        Pipe {
            read_side: false,
            shared: self.shared.clone(),
            non_blocking: AtomicBool::new(self.nonblocking()),
        }
    }

    #[cfg(all(test, axtest))]
    fn write_without_sigpipe_for_test(&self, src: &mut IoSrc) -> StarryResult<usize> {
        self.write_with_broken_pipe_handler(src, || {})
    }
}

fn rounded_pipe_size(size: usize) -> StarryResult<usize> {
    let page_count = size.div_ceil(PAGE_SIZE_4K).max(1);
    let page_count = page_count
        .checked_next_power_of_two()
        .ok_or(StarryError::InvalidInput)?;
    let size = page_count
        .checked_mul(PAGE_SIZE_4K)
        .ok_or(StarryError::InvalidInput)?;
    if size > RING_BUFFER_MAX_SIZE {
        return Err(StarryError::OperationNotPermitted);
    }
    Ok(size)
}

#[cfg(all(test, axtest))]
fn peer_close_with_multiple_readers_is_visible_for_test() -> bool {
    let (read_end, write_end) = Pipe::new();
    let second_reader = read_end.duplicate_read_end_for_test();

    drop(write_end);

    read_end.poll().contains(IoEvents::HUP) && second_reader.poll().contains(IoEvents::HUP)
}

#[cfg(all(test, axtest))]
fn resize_rejects_oversized_pipe_for_test() -> bool {
    let (read_end, _write_end) = Pipe::new();
    read_end.resize(1024 * 1024 + 1).is_err()
}

#[cfg(all(test, axtest))]
fn pipe_linux_io_semantics_hold_for_test() -> bool {
    let null_io_matches = {
        let (read_end, write_end) = Pipe::new();
        read_end.set_nonblocking(true).ok();
        write_end.set_nonblocking(true).ok();

        let mut empty_dst: &mut [u8] = &mut [];
        let null_read = read_end.read(&mut empty_dst as &mut dyn super::WriteBuf);
        drop(read_end);
        let mut empty_src: &[u8] = &[];
        let null_write =
            write_end.write_without_sigpipe_for_test(&mut empty_src as &mut dyn super::ReadBuf);

        matches!(null_read, Ok(0)) && matches!(null_write, Ok(0))
    };

    let atomic_write_and_poll_match = {
        let mut state = PipeState::new(PIPE_BUF);
        let initial = [b'a'; 4000];
        let mut initial_src: &[u8] = &initial;
        let initial_write = state.append_from(&mut initial_src as &mut dyn super::ReadBuf);
        let atomic_can_commit = state.can_merge(200) || state.has_free_buffer();

        matches!(initial_write, Ok(written) if written == initial.len())
            && !atomic_can_commit
            && state.buffer.occupied_len() == initial.len()
            && !state.poll_events(false).contains(IoEvents::OUT)
    };

    let closed_reader_poll_matches = {
        let mut state = PipeState::new(PIPE_BUF);
        state.readers = 0;
        state
            .poll_events(false)
            .contains(IoEvents::OUT | IoEvents::ERR)
    };

    let duplicates_preserve_nonblocking = {
        let (read_end, write_end) = Pipe::new();
        read_end.set_nonblocking(true).ok();
        write_end.set_nonblocking(true).ok();
        read_end.duplicate_read_end_for_test().nonblocking()
            && write_end.duplicate_write_end_for_test().nonblocking()
    };

    let page_slot_fragmentation_matches = {
        let mut state = PipeState::new(2 * PIPE_BUF);
        let initial = [b'a'; 5000];
        let mut initial_src: &[u8] = &initial;
        let first_write = state.append_from(&mut initial_src as &mut dyn super::ReadBuf);
        let second_write = state.append_from(&mut initial_src as &mut dyn super::ReadBuf);
        unsafe { state.buffer.advance_read_index(1000) };
        state.consume(1000);
        let shrink_is_busy = 1 < state.buffers.len();
        let atomic_can_commit = state.can_merge(4000) || state.has_free_buffer();

        matches!(first_write, Ok(PIPE_BUF))
            && matches!(second_write, Ok(written) if written == initial.len() - PIPE_BUF)
            && !state.poll_events(false).contains(IoEvents::OUT)
            && shrink_is_busy
            && !atomic_can_commit
    };

    let published_readiness_matches = {
        let (read_end, write_end) = Pipe::new();
        let resized_to_one_slot = write_end.resize(PIPE_BUF).is_ok();
        let input = [b'p'; PIPE_BUF];
        let mut src: &[u8] = &input;
        let write = write_end.write_without_sigpipe_for_test(&mut src as &mut dyn super::ReadBuf);
        let data_is_ready = read_end.poll().contains(IoEvents::IN);
        let full_pipe_is_not_writable = !write_end.poll().contains(IoEvents::OUT);
        let expanded = write_end.resize(2 * PIPE_BUF).is_ok();
        let expanded_pipe_is_writable = write_end.poll().contains(IoEvents::OUT);

        let mut output = [0; PIPE_BUF];
        let mut dst: &mut [u8] = &mut output;
        let read = read_end.read(&mut dst as &mut dyn super::WriteBuf);
        let drained_pipe_is_not_readable = !read_end.poll().contains(IoEvents::IN);
        drop(read_end);
        let closed_reader_is_visible = write_end.poll().contains(IoEvents::ERR);

        resized_to_one_slot
            && matches!(write, Ok(PIPE_BUF))
            && data_is_ready
            && full_pipe_is_not_writable
            && expanded
            && expanded_pipe_is_writable
            && matches!(read, Ok(PIPE_BUF))
            && drained_pipe_is_not_readable
            && closed_reader_is_visible
    };

    null_io_matches
        && atomic_write_and_poll_match
        && closed_reader_poll_matches
        && duplicates_preserve_nonblocking
        && page_slot_fragmentation_matches
        && published_readiness_matches
}

fn raise_pipe() {
    let curr = current_user_task();
    send_signal_to_process(
        curr.as_thread().proc_data.proc.pid_number(),
        Some(SignalInfo::new_kernel(Signo::SIGPIPE)),
    )
    .expect("Failed to send SIGPIPE");
}

impl FileLike for Pipe {
    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        if !self.is_read() {
            return Err(StarryError::BadFileDescriptor);
        }
        if dst.is_full() {
            return Ok(0);
        }
        #[cfg(feature = "qperf-metrics")]
        PIPE_READ_CALLS.fetch_add(1, Ordering::Relaxed);

        let mut operation = |selected_for_handoff: bool| {
            let (read, writers, wake_writers, wake_next_reader) =
                self.shared
                    .update_state(|state| -> StarryResult<(usize, usize, bool, bool)> {
                        let was_full = !state.has_free_buffer();
                        let (left, right) = state.buffer.as_slices();
                        let mut count = dst.write(left)?;
                        if count >= left.len() {
                            count += dst.write(right)?;
                        }
                        unsafe { state.buffer.advance_read_index(count) };
                        state.consume(count);
                        Ok((
                            count,
                            state.writers,
                            count > 0 && was_full && state.has_free_buffer(),
                            count > 0 && selected_for_handoff && !state.buffer.is_empty(),
                        ))
                    })?;
            if read > 0 {
                #[cfg(feature = "qperf-metrics")]
                PIPE_READ_BYTES.fetch_add(read as u64, Ordering::Relaxed);
                if wake_writers {
                    // Pipe capacity was freed before waking writers.
                    wake_pipe_waiter_sync(&self.shared.wait_tx, IoEvents::OUT);
                }
                if wake_next_reader {
                    // A selected reader transfers remaining data to the next
                    // exclusive waiter.
                    wake_pipe_waiter_sync(&self.shared.wait_rx, IoEvents::IN);
                }
                Ok(read)
            } else if writers == 0 {
                Ok(0)
            } else {
                Err(StarryError::WouldBlock)
            }
        };

        let mut selected_for_handoff = false;
        let mut wait_recorded = false;
        let mut task = None;
        loop {
            match operation(selected_for_handoff) {
                Ok(result) => return Ok(result),
                Err(error) if error.is_would_block() && self.nonblocking() => return Err(error),
                Err(error) if error.is_would_block() => {}
                Err(error) => return Err(error),
            }
            if !wait_recorded {
                #[cfg(feature = "qperf-metrics")]
                PIPE_READ_WAITS.fetch_add(1, Ordering::Relaxed);
                wait_recorded = true;
            }

            let task = task.get_or_insert_with(current_user_task);
            if task.take_interrupt() {
                return Err(StarryError::Interrupted);
            }
            selected_for_handoff = self
                .shared
                .wait_rx
                .wait_until(|| self.shared.readiness().read_wait_ready() || task.interrupted());
        }
    }

    fn write(&self, src: &mut IoSrc) -> StarryResult<usize> {
        self.write_with_broken_pipe_handler(src, raise_pipe)
    }

    fn stat(&self) -> StarryResult<Kstat> {
        Ok(Kstat {
            mode: S_IFIFO | if self.is_read() { 0o444 } else { 0o222 },
            ..Default::default()
        })
    }

    fn path(&self) -> Cow<'_, str> {
        format!("pipe:[{}]", self as *const _ as usize).into()
    }

    fn open_flags(&self) -> u32 {
        if self.is_read() { O_RDONLY } else { O_WRONLY }
    }

    fn set_nonblocking(&self, nonblocking: bool) -> StarryResult {
        self.non_blocking.store(nonblocking, Ordering::Release);
        Ok(())
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn ioctl(
        &self,
        current: &crate::task::UserTaskRef,
        cmd: u32,
        arg: usize,
    ) -> crate::StarryResult<usize> {
        match cmd {
            FIONREAD => {
                (arg as *mut u32).vm_write(
                    current,
                    self.shared.state.lock().buffer.occupied_len() as u32,
                )?;
                Ok(0)
            }
            _ => Err(StarryError::NotATty),
        }
    }
}

impl Pollable for Pipe {
    fn poll(&self) -> IoEvents {
        // Linux reports POLLOUT when the pipe has a free PIPE_BUF-sized slot,
        // independently of whether the reader has already closed.
        self.shared.readiness().poll_events(self.read_side)
    }

    unsafe fn register_shared(
        &self,
        sink: &mut dyn axpoll::SharedRegistrationSink,
        events: IoEvents,
    ) {
        self.register_poll_source(events, |poll, interests| unsafe {
            sink.register_shared(poll, interests)
        });
    }

    unsafe fn register_exclusive(
        &self,
        sink: &mut dyn axpoll::ExclusiveRegistrationSink,
        events: IoEvents,
    ) {
        self.register_poll_source(events, |poll, interests| unsafe {
            sink.register_exclusive(poll, interests)
        });
    }
}

impl Pipe {
    fn register_poll_source(
        &self,
        events: IoEvents,
        register: impl FnOnce(&dyn PollSource, IoEvents),
    ) {
        // Linux publishes poll_usage for every pipe_poll() attempt, including
        // exclusive consumers, so non-empty writes keep notifying pollers.
        self.shared.poll_usage.store(true, Ordering::Release);
        let read_ready = events.intersects(IoEvents::IN | IoEvents::RDNORM);
        let write_ready = events.intersects(IoEvents::OUT | IoEvents::WRNORM);
        let mut interests = if self.read_side {
            events & IoEvents::HUP
        } else {
            events & IoEvents::ERR
        };
        if self.read_side && read_ready {
            interests.insert(IoEvents::IN);
            interests.insert(IoEvents::HUP);
        }
        if !self.read_side && write_ready {
            interests.insert(IoEvents::OUT);
            interests.insert(IoEvents::ERR);
        }
        if interests.is_empty() {
            return;
        }
        if self.read_side {
            register(&self.shared.wait_rx, interests);
        } else {
            register(&self.shared.wait_tx, interests);
        }
    }
}

#[cfg(all(test, axtest))]
fn pipe_resize_rounding_and_state_rules_hold_for_test() -> bool {
    let (read_end, _write_end) = Pipe::new();

    // Initial capacity is the default 64 KiB ring buffer.
    let initial_capacity = read_end.capacity();

    // Newly allocated pipe has one reader and one writer.
    read_end.is_read()
        && !read_end.is_write()
        // Resizing to the current capacity is a no-op success.
        && read_end.resize(initial_capacity).is_ok()
        && read_end.capacity() == initial_capacity
        // Round up to the next power-of-two page multiple: 4097 -> 8192.
        && read_end.resize(4097).is_ok()
        && read_end.capacity() == 8192
        // Sub-page sizes are rounded up to a single page (4096).
        && read_end.resize(1).is_ok()
        && read_end.capacity() == 4096
        // Sizes above RING_BUFFER_MAX_SIZE (1 MiB) are rejected.
        && read_end.resize(1024 * 1024 + 1).is_err()
        // Zero-sized resize rounds up to one page (no InvalidInput).
        && read_end.resize(0).is_ok()
        && read_end.capacity() == 4096
}

#[cfg(all(test, axtest))]
mod tests {
    use alloc::{string::ToString, sync::Arc, task::Wake};
    use core::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        task::Waker,
    };

    use ax_std::os::arceos::task::{
        self as scheduler, SchedulePolicy, SwitchReason, ThreadExtension, ThreadExtensionOps,
        ThreadHandle, ThreadId,
    };
    use axpoll::{ExclusiveConsumer, PollRegistrar, PollSource, Pollable, RegistrationMode};
    use ringbuf::traits::Consumer;

    use super::{
        IoEvents, PIPE_BUF, Pipe, PipeState, PipeWaitSet, PipeWaitTarget, wake_pipe_waiter_sync,
    };

    static DIRECT_READY: AtomicBool = AtomicBool::new(false);
    static DIRECT_WAIT_ARMED: AtomicBool = AtomicBool::new(false);
    static DIRECT_BLOCKED: AtomicBool = AtomicBool::new(false);
    static DIRECT_WOKEN: AtomicBool = AtomicBool::new(false);

    unsafe extern "Rust" fn ignore_switch_in(
        _data: usize,
        _thread: ThreadId,
        _policy: SchedulePolicy,
    ) {
    }

    unsafe extern "Rust" fn observe_block(_data: usize, _thread: ThreadId, reason: SwitchReason) {
        if reason == SwitchReason::Blocked && DIRECT_WAIT_ARMED.swap(false, Ordering::AcqRel) {
            DIRECT_BLOCKED.store(true, Ordering::Release);
        }
    }

    unsafe extern "Rust" fn ignore_thread_event(_data: usize, _thread: ThreadId) {}

    unsafe extern "Rust" fn ignore_extension_drop(_data: usize) {}

    static BLOCK_OBSERVER_OPS: ThreadExtensionOps = ThreadExtensionOps {
        on_switch_in: ignore_switch_in,
        on_switch_out: observe_block,
        on_exit: ignore_thread_event,
        on_deadline_overrun: ignore_thread_event,
        drop: ignore_extension_drop,
    };

    struct CountWake(AtomicUsize);

    impl Wake for CountWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct WakeOrder {
        next: AtomicUsize,
        slots: [AtomicUsize; 3],
    }

    struct OrderedWake {
        id: usize,
        order: Arc<WakeOrder>,
    }

    impl Wake for OrderedWake {
        fn wake(self: Arc<Self>) {
            let index = self.order.next.fetch_add(1, Ordering::AcqRel);
            self.order
                .slots
                .get(index)
                .expect("pipe wake callback count exceeded the expected budget")
                .store(self.id, Ordering::Release);
        }
    }

    fn wait_for(flag: &AtomicBool, message: &str) {
        for _ in 0..1_000_000 {
            if flag.load(Ordering::Acquire) {
                return;
            }
            ax_std::thread::yield_now();
        }
        panic!("{message}");
    }

    fn spawn_direct_waiter(waiters: Arc<PipeWaitSet>) -> ThreadHandle {
        DIRECT_WAIT_ARMED.store(false, Ordering::Release);
        DIRECT_BLOCKED.store(false, Ordering::Release);
        DIRECT_WOKEN.store(false, Ordering::Release);
        DIRECT_READY.store(false, Ordering::Release);

        // SAFETY: the extension owns no data and only publishes bounded atomic
        // observations from scheduler switch callbacks.
        let extension = unsafe { ThreadExtension::new(0, &BLOCK_OBSERVER_OPS) };
        // SAFETY: unique ownership of `extension` is transferred exactly once.
        let direct = unsafe {
            scheduler::spawn_raw_with_extension(
                move || {
                    DIRECT_WAIT_ARMED.store(true, Ordering::Release);
                    waiters.wait_until(|| DIRECT_READY.load(Ordering::Acquire));
                    DIRECT_WOKEN.store(true, Ordering::Release);
                },
                "pipe-direct-exclusive-waiter".to_string(),
                256 * 1024,
                Some(extension),
            )
        }
        .expect("failed to spawn direct pipe waiter");
        wait_for(&DIRECT_BLOCKED, "direct pipe waiter did not block");
        direct
    }

    #[axtest::axtest]
    fn peer_close_with_multiple_readers_is_visible() {
        assert!(super::peer_close_with_multiple_readers_is_visible_for_test());
    }

    #[axtest::axtest]
    fn resize_rejects_oversized_pipe() {
        assert!(super::resize_rejects_oversized_pipe_for_test());
    }

    #[axtest::axtest]
    fn pipe_linux_io_semantics_hold() {
        assert!(super::pipe_linux_io_semantics_hold_for_test());
    }

    #[axtest::axtest]
    fn partial_pipe_buffer_cannot_merge_past_its_linux_page_boundary() {
        let mut state = PipeState::new(2 * PIPE_BUF);
        let initial = [b'a'; PIPE_BUF];
        let mut src: &[u8] = &initial;
        assert!(
            matches!(
                state.append_from(&mut src as &mut dyn super::super::ReadBuf),
                Ok(written) if written == PIPE_BUF
            ),
            "initial Linux pipe buffer must fill one complete page slot"
        );

        const CONSUMED: usize = 2096;
        unsafe { state.buffer.advance_read_index(CONSUMED) };
        state.consume(CONSUMED);

        assert!(
            !state.can_merge(2000),
            "Linux pipe_buffer offset must remain part of the merge boundary after a partial read"
        );
    }

    #[axtest::axtest]
    fn exclusive_pipe_poll_registration_marks_linux_poll_usage() {
        let (read_end, _write_end) = Pipe::new();
        let wakes = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(wakes);
        let mut registrar = PollRegistrar::<ExclusiveConsumer>::new(&waker);

        unsafe { read_end.register_exclusive(&mut registrar, IoEvents::IN) };

        assert!(
            read_end.shared.poll_usage.load(Ordering::Acquire),
            "every Linux pipe_poll registration must publish poll_usage"
        );
    }

    #[axtest::axtest]
    fn nonempty_pipe_batches_poll_wake_until_write_completion() {
        let wake_readers =
            super::pipe_write_reader_wake_due(false, true, super::PipeWriteWakePhase::BeforeWait);
        assert!(
            !wake_readers,
            "Linux defers poll_usage wake for a nonempty pipe until write completion"
        );
    }

    #[axtest::axtest]
    fn pipe_resize_rounding_and_state_rules_hold() {
        assert!(super::pipe_resize_rounding_and_state_rules_hold_for_test());
    }

    #[axtest::axtest]
    fn direct_and_epollexclusive_waiters_share_one_wake_budget() {
        let waiters = Arc::new(PipeWaitSet::new());
        let direct = spawn_direct_waiter(Arc::clone(&waiters));

        let poll_wakes = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&poll_wakes));
        // SAFETY: the registration and source outlive this wake transaction.
        let registration = unsafe {
            PollSource::register(
                waiters.as_ref(),
                &waker,
                IoEvents::IN,
                RegistrationMode::Exclusive,
            )
        }
        .expect("exclusive poll registration must succeed");

        DIRECT_READY.store(true, Ordering::Release);
        wake_pipe_waiter_sync(waiters.as_ref(), IoEvents::IN);
        wait_for(&DIRECT_WOKEN, "direct pipe waiter was not selected");
        scheduler::join_thread(direct).expect("direct pipe waiter must exit cleanly");
        drop(registration);

        assert_eq!(
            poll_wakes.0.load(Ordering::Acquire),
            0,
            "one pipe event must not consume a second EPOLLEXCLUSIVE quota"
        );
    }

    #[axtest::axtest]
    fn shared_poll_callbacks_precede_exclusive_in_linux_queue_order() {
        let waiters = PipeWaitSet::new();
        let order = Arc::new(WakeOrder {
            next: AtomicUsize::new(0),
            slots: core::array::from_fn(|_| AtomicUsize::new(0)),
        });
        let waker = |id| {
            Waker::from(Arc::new(OrderedWake {
                id,
                order: Arc::clone(&order),
            }))
        };

        // Linux inserts non-exclusive poll waiters at the queue head, so the
        // later shared registration runs first. Exclusive waiters stay at the
        // tail and consume the single wake quota only after all shared entries.
        // SAFETY: every registration and waker outlives this wake transaction.
        let shared_one = unsafe {
            PollSource::register(&waiters, &waker(1), IoEvents::IN, RegistrationMode::Shared)
        }
        .expect("first shared registration must succeed");
        // SAFETY: every registration and waker outlives this wake transaction.
        let shared_two = unsafe {
            PollSource::register(&waiters, &waker(2), IoEvents::IN, RegistrationMode::Shared)
        }
        .expect("second shared registration must succeed");
        // SAFETY: every registration and waker outlives this wake transaction.
        let exclusive = unsafe {
            PollSource::register(
                &waiters,
                &waker(3),
                IoEvents::IN,
                RegistrationMode::Exclusive,
            )
        }
        .expect("exclusive registration must succeed");

        wake_pipe_waiter_sync(&waiters, IoEvents::IN);

        assert_eq!(order.next.load(Ordering::Acquire), 3);
        assert_eq!(
            order
                .slots
                .each_ref()
                .map(|slot| slot.load(Ordering::Acquire)),
            [2, 1, 3],
            "pipe wake order must match Linux waitqueue insertion and quota rules"
        );
        assert!(shared_one.was_notified());
        assert!(shared_two.was_notified());
        assert!(exclusive.was_notified());
    }

    #[axtest::axtest]
    fn earlier_epollexclusive_waiter_precedes_a_later_direct_waiter() {
        let waiters = Arc::new(PipeWaitSet::new());
        let poll_wakes = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&poll_wakes));
        // SAFETY: the registration and source outlive both wake transactions.
        let registration = unsafe {
            PollSource::register(
                waiters.as_ref(),
                &waker,
                IoEvents::IN,
                RegistrationMode::Exclusive,
            )
        }
        .expect("exclusive poll registration must succeed");
        let direct = spawn_direct_waiter(Arc::clone(&waiters));

        DIRECT_READY.store(true, Ordering::Release);
        wake_pipe_waiter_sync(waiters.as_ref(), IoEvents::IN);
        assert_eq!(poll_wakes.0.load(Ordering::Acquire), 1);
        assert!(
            !DIRECT_WOKEN.load(Ordering::Acquire),
            "later direct waiter must not bypass the older exclusive registration"
        );

        wake_pipe_waiter_sync(waiters.as_ref(), IoEvents::IN);
        wait_for(&DIRECT_WOKEN, "second wake did not select direct waiter");
        scheduler::join_thread(direct).expect("direct pipe waiter must exit cleanly");
        drop(registration);
    }

    #[axtest::axtest]
    fn selected_epollexclusive_notification_precedes_registration_drop() {
        let waiters = PipeWaitSet::new();
        let poll_wakes = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&poll_wakes));
        // SAFETY: the source remains alive until the selected callback runs.
        let registration = unsafe {
            PollSource::register(&waiters, &waker, IoEvents::IN, RegistrationMode::Exclusive)
        }
        .expect("exclusive poll registration must succeed");
        let boundary = waiters.state.lock().next_id;

        let selected = waiters
            .take_next_exclusive(IoEvents::IN, boundary, None)
            .expect("registered exclusive waiter must be selected");
        assert!(
            registration.was_notified(),
            "selection must publish notification under the registration lock"
        );
        drop(registration);

        let PipeWaitTarget::Poll { waker, .. } = selected.target else {
            panic!("selected registration must retain its poll callback");
        };
        waker.wake();
        assert_eq!(poll_wakes.0.load(Ordering::Acquire), 1);
    }

    #[axtest::axtest]
    fn condition_race_preserves_linux_exclusive_handoff() {
        let waiters = PipeWaitSet::new();

        assert!(
            waiters.wait_until(|| true),
            "returning from an exclusive wait must hand remaining readiness to the next waiter"
        );
    }
}
