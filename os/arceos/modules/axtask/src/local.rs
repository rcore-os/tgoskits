//! Local task runtime helpers.
//!
//! These primitives are intended for device/runtime worker threads that want to
//! run several cooperative futures inside one ordinary `ax-task` thread. The
//! outer scheduler remains the traditional thread scheduler; the local executor
//! only multiplexes futures while the host thread is running.

use alloc::sync::Arc;
use core::{
    future::Future,
    task::{Context, Poll},
};

use bare_task::{LocalExecutorCore, LocalSpawnerCore};
pub use bare_task::{RuntimeEventSeq, RuntimeEventValue, SpawnLocalError};

use crate::{HardIrqSignal, HardIrqWaker, WaitQueue};

/// Sticky event source for IRQ/deferred runtime code.
///
/// `RuntimeEvent` stores readiness as a monotonically increasing sequence plus
/// coalesced event bits. Wakers are only a hint to re-poll; callers must still
/// consume the published state.
pub struct RuntimeEvent {
    core: bare_task::RuntimeEventCore,
    notify: HardIrqSignal,
}

impl RuntimeEvent {
    /// Creates an empty event source.
    pub const fn new() -> Self {
        Self {
            core: bare_task::RuntimeEventCore::new(),
            notify: HardIrqSignal::new(),
        }
    }

    /// Returns the latest sequence.
    pub fn seq(&self) -> RuntimeEventValue {
        self.core.seq()
    }

    /// Returns whether at least one event has been published.
    pub fn has_unseen_events(&self) -> bool {
        self.core.has_unseen_events()
    }

    /// Returns whether the event sequence has changed from `observed`.
    pub fn has_changed(&self, observed: &RuntimeEventSeq) -> bool {
        self.core.has_changed(observed)
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
        let seq = self.core.publish(bits);
        self.notify.notify();
        seq
    }

    /// Publishes event bits from hard IRQ context.
    pub fn publish_from_irq(&self, bits: u64) -> RuntimeEventValue {
        self.core.publish_state(bits)
    }

    /// Publishes event bits from hard IRQ context and wakes a host task through
    /// the IRQ-safe task wake path.
    pub fn publish_from_irq_with(
        &self,
        bits: u64,
        waker: &HardIrqWaker,
    ) -> (RuntimeEventValue, crate::WakeResult) {
        let seq = self.core.publish_state(bits);
        let wake = waker.wake_from_irq(bits);
        (seq, wake)
    }

    /// Takes all coalesced event bits.
    pub fn take_bits(&self) -> u64 {
        self.core.take_bits()
    }

    /// Polls until the event sequence changes from `observed`.
    ///
    /// The registration protocol is: check sequence, register, then re-check.
    /// This mirrors the standard async lost-wake prevention pattern.
    pub fn poll_changed(&self, observed: &RuntimeEventSeq, cx: &mut Context<'_>) -> Poll<u64> {
        self.core.poll_changed(observed, cx)
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
        self.core.wake_waiters();
    }
}

impl Default for RuntimeEvent {
    fn default() -> Self {
        Self::new()
    }
}

/// Single-threaded future executor hosted by an ordinary `ax-task` thread.
#[derive(Clone)]
pub struct LocalExecutor {
    core: LocalExecutorCore,
    wait: Arc<WaitQueue>,
}

impl LocalExecutor {
    /// Creates an empty local executor.
    pub fn new() -> Self {
        let wait = Arc::new(WaitQueue::new());
        let wait_for_pend = wait.clone();
        let host_waker = crate::try_current_task_waker();
        let core = LocalExecutorCore::new(Arc::new(move || {
            if let Some(waker) = &host_waker {
                let _ = waker.wake(0);
            }
            wait_for_pend.notify_one(true);
        }));
        Self { core, wait }
    }

    /// Returns a spawner tied to this executor.
    pub fn spawner(&self) -> LocalSpawner {
        LocalSpawner {
            core: self.core.spawner(),
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
            self.core.poll_ready();
            if external_ready() || !self.has_live_tasks() {
                self.leave();
                return;
            }
            self.wait
                .wait_until(|| self.core.has_ready_tasks() || external_ready());
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
            self.core.poll_ready();
            if external_ready() {
                event.wake_waiters_deferred();
                if self.core.has_ready_tasks() {
                    continue;
                }
                self.leave();
                return;
            }
            if !self.has_live_tasks() {
                self.leave();
                return;
            }
            event.wait_until(|| self.core.has_ready_tasks() || external_ready());
            event.wake_waiters_deferred();
        }
    }

    fn run_ready_tasks(&self) {
        self.enter();
        self.core.poll_ready();
        self.leave();
    }

    fn enter(&self) {
        self.core.enter();
    }

    fn leave(&self) {
        self.core.leave();
    }

    fn has_live_tasks(&self) -> bool {
        self.core.has_live_tasks()
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
    core: LocalSpawnerCore,
}

impl LocalSpawner {
    /// Spawns a future on the local executor.
    pub fn spawn_local<F>(&self, future: F) -> Result<(), SpawnLocalError>
    where
        F: Future<Output = ()> + 'static,
    {
        self.core.spawn_local(future)
    }
}
