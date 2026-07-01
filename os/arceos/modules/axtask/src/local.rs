//! Local task runtime helpers.
//!
//! These primitives are intended for device/runtime worker threads that want to
//! run several cooperative futures inside one ordinary `ax-task` thread. The
//! outer scheduler remains the traditional thread scheduler; the local executor
//! only multiplexes futures while the host thread is running.

use alloc::{
    boxed::Box,
    collections::VecDeque,
    sync::{Arc, Weak},
    task::Wake,
    vec::Vec,
};
use core::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use ax_kspin::SpinNoIrq;

use crate::{IrqNotify, IrqTaskWaker, WaitQueue};

/// Mutable sequence cursor for [`RuntimeEvent`].
#[derive(Debug, Default)]
pub struct RuntimeEventSeq(AtomicU64);

impl RuntimeEventSeq {
    /// Creates a zero sequence token.
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Returns the raw sequence value.
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    /// Updates the observed sequence.
    pub fn update(&self, seq: RuntimeEventValue) {
        self.0.store(seq.get(), Ordering::Release);
    }
}

/// Published runtime event sequence value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeEventValue(u64);

impl RuntimeEventValue {
    /// Returns the raw sequence value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Default)]
struct RuntimeEventWaiters {
    wakers: Vec<Waker>,
}

/// Sticky event source for IRQ/deferred runtime code.
///
/// `RuntimeEvent` stores readiness as a monotonically increasing sequence plus
/// coalesced event bits. Wakers are only a hint to re-poll; callers must still
/// consume the published state.
pub struct RuntimeEvent {
    seq: AtomicU64,
    bits: AtomicU64,
    waiters: SpinNoIrq<RuntimeEventWaiters>,
    notify: IrqNotify,
}

impl RuntimeEvent {
    /// Creates an empty event source.
    pub const fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            bits: AtomicU64::new(0),
            waiters: SpinNoIrq::new(RuntimeEventWaiters { wakers: Vec::new() }),
            notify: IrqNotify::new(),
        }
    }

    /// Returns the latest sequence.
    pub fn seq(&self) -> RuntimeEventValue {
        RuntimeEventValue(self.seq.load(Ordering::Acquire))
    }

    /// Returns whether at least one event has been published.
    pub fn has_unseen_events(&self) -> bool {
        self.seq.load(Ordering::Acquire) != 0
    }

    /// Returns whether the event sequence has changed from `observed`.
    pub fn has_changed(&self, observed: &RuntimeEventSeq) -> bool {
        self.seq.load(Ordering::Acquire) != observed.get()
    }

    /// Blocks until the sequence differs from `observed`, then updates it.
    #[track_caller]
    pub fn wait_changed(&self, observed: &RuntimeEventSeq) -> RuntimeEventValue {
        self.wait_until(|| self.has_changed(observed));
        let seq = self.seq();
        observed.update(seq);
        seq
    }

    /// Publishes event bits from task/deferred context.
    pub fn publish(&self, bits: u64) -> RuntimeEventValue {
        self.publish_inner(bits, false)
    }

    /// Publishes event bits from hard IRQ context.
    pub fn publish_from_irq(&self, bits: u64) -> RuntimeEventValue {
        self.publish_state(bits)
    }

    /// Publishes event bits from hard IRQ context and wakes a host task through
    /// the IRQ-safe task wake path.
    pub fn publish_from_irq_with(
        &self,
        bits: u64,
        waker: &IrqTaskWaker,
    ) -> (RuntimeEventValue, crate::IrqWakeResult) {
        let seq = self.publish_state(bits);
        let wake = waker.wake_from_irq(bits);
        (seq, wake)
    }

    fn publish_state(&self, bits: u64) -> RuntimeEventValue {
        if bits != 0 {
            self.bits.fetch_or(bits, Ordering::AcqRel);
        }
        RuntimeEventValue(self.seq.fetch_add(1, Ordering::AcqRel) + 1)
    }

    fn publish_inner(&self, bits: u64, from_irq: bool) -> RuntimeEventValue {
        debug_assert!(
            !from_irq,
            "hard IRQ publishers must use publish_from_irq or publish_from_irq_with",
        );
        let seq = self.publish_state(bits);
        self.notify.notify();
        self.wake_waiters();
        seq
    }

    /// Takes all coalesced event bits.
    pub fn take_bits(&self) -> u64 {
        self.bits.swap(0, Ordering::AcqRel)
    }

    /// Polls until the event sequence changes from `observed`.
    ///
    /// The registration protocol is: check sequence, register, then re-check.
    /// This mirrors the standard async lost-wake prevention pattern.
    pub fn poll_changed(&self, observed: &RuntimeEventSeq, cx: &mut Context<'_>) -> Poll<u64> {
        let seq = self.seq.load(Ordering::Acquire);
        if seq != observed.get() {
            observed.0.store(seq, Ordering::Release);
            return Poll::Ready(seq);
        }

        self.register_waker(cx.waker());

        let seq = self.seq.load(Ordering::Acquire);
        if seq != observed.get() {
            observed.0.store(seq, Ordering::Release);
            Poll::Ready(seq)
        } else {
            Poll::Pending
        }
    }

    /// Blocks the current host thread until `condition` becomes true or a
    /// runtime event is published.
    #[track_caller]
    pub fn wait_until(&self, condition: impl Fn() -> bool) {
        self.notify.wait_until(condition);
    }

    /// Wakes futures registered with [`poll_changed`](Self::poll_changed).
    ///
    /// IRQ publishers intentionally do not call arbitrary wakers. Device
    /// runtime threads should call this after the IRQ notification has returned
    /// them to task context.
    pub fn wake_waiters_deferred(&self) {
        self.wake_waiters();
    }

    fn register_waker(&self, waker: &Waker) {
        let mut waiters = self.waiters.lock();
        if waiters
            .wakers
            .iter()
            .any(|existing| existing.will_wake(waker))
        {
            return;
        }
        waiters.wakers.push(waker.clone());
    }

    fn wake_waiters(&self) {
        let waiters = {
            let mut waiters = self.waiters.lock();
            core::mem::take(&mut waiters.wakers)
        };
        for waker in waiters {
            waker.wake();
        }
    }
}

impl Default for RuntimeEvent {
    fn default() -> Self {
        Self::new()
    }
}

type LocalFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

struct LocalTask {
    future: SpinNoIrq<Option<LocalFuture>>,
    queued: AtomicBool,
    completed: AtomicBool,
    executor: Weak<LocalExecutorInner>,
}

impl LocalTask {
    fn poll(self: &Arc<Self>) {
        if self.completed.load(Ordering::Acquire) {
            return;
        }
        let waker = Waker::from(self.clone());
        let mut cx = Context::from_waker(&waker);
        let completed = {
            let mut future_slot = self.future.lock();
            let Some(future) = future_slot.as_mut() else {
                return;
            };
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(()) => {
                    *future_slot = None;
                    true
                }
                Poll::Pending => false,
            }
        };
        if completed {
            self.completed.store(true, Ordering::Release);
            if let Some(executor) = self.executor.upgrade() {
                executor.task_count.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }

    fn enqueue(self: &Arc<Self>) {
        if self.completed.load(Ordering::Acquire) {
            return;
        }
        if self.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(executor) = self.executor.upgrade() {
            executor.ready.lock().push_back(self.clone());
            executor.wake_host();
        }
    }
}

impl Wake for LocalTask {
    fn wake(self: Arc<Self>) {
        self.enqueue();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.enqueue();
    }
}

struct LocalExecutorInner {
    ready: SpinNoIrq<VecDeque<Arc<LocalTask>>>,
    wait: WaitQueue,
    host_waker: Option<IrqTaskWaker>,
    active: AtomicBool,
    task_count: AtomicUsize,
}

impl LocalExecutorInner {
    fn wake_host(&self) {
        if let Some(waker) = &self.host_waker {
            let _ = waker.wake(0);
        }
        self.wait.notify_one(true);
    }

    fn has_ready_tasks(&self) -> bool {
        !self.ready.lock().is_empty()
    }
}

/// Single-threaded future executor hosted by an ordinary `ax-task` thread.
#[derive(Clone)]
pub struct LocalExecutor {
    inner: Arc<LocalExecutorInner>,
}

impl LocalExecutor {
    /// Creates an empty local executor.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(LocalExecutorInner {
                ready: SpinNoIrq::new(VecDeque::new()),
                wait: WaitQueue::new(),
                host_waker: crate::try_current_irq_task_waker(),
                active: AtomicBool::new(false),
                task_count: AtomicUsize::new(0),
            }),
        }
    }

    /// Returns a spawner tied to this executor.
    pub fn spawner(&self) -> LocalSpawner {
        LocalSpawner {
            inner: self.inner.clone(),
        }
    }

    /// Polls all ready tasks until no ready task remains.
    pub fn run_until_idle(&self) {
        self.run_ready_tasks();
    }

    /// Runs ready tasks, blocking the host thread when all local tasks are
    /// pending and `external_ready` is false.
    ///
    /// The function returns once no local task is ready and `external_ready`
    /// evaluates true. Runtime users normally drain the external event and call
    /// this again from their worker loop.
    #[track_caller]
    pub fn run_until_idle_with(&self, external_ready: impl Fn() -> bool) {
        self.enter();
        loop {
            self.run_ready_tasks_locked();
            if external_ready() || !self.has_live_tasks() {
                self.leave();
                return;
            }
            self.inner
                .wait
                .wait_until(|| self.inner.has_ready_tasks() || external_ready());
        }
    }

    /// Runs ready tasks and sleeps on a [`RuntimeEvent`] when all local tasks
    /// are pending.
    ///
    /// When the runtime event is published from IRQ context, this method returns
    /// to task context first, wakes futures registered on the event, and then
    /// polls the ready queue. This keeps hard IRQ callbacks from invoking
    /// arbitrary `Waker` implementations.
    #[track_caller]
    pub fn run_until_event(&self, event: &RuntimeEvent, external_ready: impl Fn() -> bool) {
        self.enter();
        loop {
            self.run_ready_tasks_locked();
            if external_ready() {
                event.wake_waiters_deferred();
                if self.inner.has_ready_tasks() {
                    continue;
                }
                self.leave();
                return;
            }
            if !self.has_live_tasks() {
                self.leave();
                return;
            }
            event.wait_until(|| self.inner.has_ready_tasks() || external_ready());
            event.wake_waiters_deferred();
        }
    }

    fn run_ready_tasks(&self) {
        self.enter();
        self.run_ready_tasks_locked();
        self.leave();
    }

    fn run_ready_tasks_locked(&self) {
        while let Some(task) = self.inner.ready.lock().pop_front() {
            task.queued.store(false, Ordering::Release);
            task.poll();
        }
    }

    fn enter(&self) {
        assert!(
            !self.inner.active.swap(true, Ordering::AcqRel),
            "local executor cannot be run reentrantly"
        );
    }

    fn leave(&self) {
        self.inner.active.store(false, Ordering::Release);
    }

    fn has_live_tasks(&self) -> bool {
        self.inner.task_count.load(Ordering::Acquire) != 0
    }
}

impl Default for LocalExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle used to spawn futures onto a [`LocalExecutor`].
#[derive(Clone)]
pub struct LocalSpawner {
    inner: Arc<LocalExecutorInner>,
}

impl LocalSpawner {
    /// Spawns a future on the local executor.
    pub fn spawn_local<F>(&self, future: F) -> Result<(), SpawnLocalError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let task = Arc::new(LocalTask {
            future: SpinNoIrq::new(Some(Box::pin(future))),
            queued: AtomicBool::new(true),
            completed: AtomicBool::new(false),
            executor: Arc::downgrade(&self.inner),
        });
        self.inner.task_count.fetch_add(1, Ordering::AcqRel);
        self.inner.ready.lock().push_back(task);
        self.inner.wake_host();
        Ok(())
    }
}

/// Error returned by [`LocalSpawner::spawn_local`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnLocalError;
