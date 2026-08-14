use alloc::{collections::BTreeMap, sync::Arc};
use core::{
    pin::Pin,
    sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
    time::Duration,
};

use ax_hal::time::{TimeValue, monotonic_time, wall_time};
use futures_util::{FutureExt, select_biased};

use crate::sync::SpinLock;

static NEXT_TIMER_KEY: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TimerKey {
    deadline: TimeValue,
    key: u64,
}

struct TimerState {
    owner_cpu: AtomicUsize,
    waker: SpinLock<Option<Waker>>,
}

impl TimerState {
    fn new(owner_cpu: usize) -> Self {
        Self {
            owner_cpu: AtomicUsize::new(owner_cpu),
            waker: SpinLock::new(None),
        }
    }

    fn register(&self, owner_cpu: usize, waker: &Waker) {
        let mut registered = self.waker.lock_irqsave();
        self.owner_cpu.store(owner_cpu, Ordering::Release);
        *registered = Some(waker.clone());
    }

    fn take_waker(&self, owner_cpu: usize) -> Option<Waker> {
        let mut registered = self.waker.lock_irqsave();
        if self.owner_cpu.load(Ordering::Acquire) == owner_cpu {
            registered.take()
        } else {
            None
        }
    }

    fn cancel(&self) {
        let _ = self.waker.lock_irqsave().take();
    }
}

struct TimerRuntime {
    wheel: BTreeMap<TimerKey, Arc<TimerState>>,
}

impl TimerRuntime {
    const fn new() -> Self {
        TimerRuntime {
            wheel: BTreeMap::new(),
        }
    }

    fn add(&mut self, deadline: TimeValue, owner_cpu: usize) -> Option<TimerFuture> {
        if deadline <= monotonic_time() {
            return None;
        }

        let key = TimerKey {
            deadline,
            key: NEXT_TIMER_KEY.fetch_add(1, Ordering::Relaxed),
        };
        let state = Arc::new(TimerState::new(owner_cpu));
        self.wheel.insert(key, Arc::clone(&state));
        Some(TimerFuture { key, state })
    }

    fn ensure_registered(&mut self, key: TimerKey, state: &Arc<TimerState>) {
        self.wheel.entry(key).or_insert_with(|| Arc::clone(state));
    }

    fn cancel(&mut self, key: &TimerKey) {
        let _ = self.wheel.remove(key);
    }

    #[cfg(feature = "irq")]
    fn next_deadline(&self, owner_cpu: usize) -> Option<TimeValue> {
        self.wheel
            .iter()
            .find(|(_, state)| state.owner_cpu.load(Ordering::Acquire) == owner_cpu)
            .map(|(key, _)| key.deadline)
    }

    fn wake(&mut self, owner_cpu: usize) {
        if self.wheel.is_empty() {
            return;
        }

        let now = monotonic_time();

        let pending = self.wheel.split_off(&TimerKey {
            deadline: now,
            key: u64::MAX,
        });

        let expired = core::mem::replace(&mut self.wheel, pending);
        for (_, state) in expired {
            if let Some(waker) = state.take_waker(owner_cpu) {
                waker.wake();
            }
        }
    }
}

percpu_static! {
    TIMER_RUNTIME: TimerRuntime = TimerRuntime::new(),
}

#[allow(dead_code)]
pub(crate) fn check_timer_events() {
    with_current(|runtime| runtime.wake(ax_hal::percpu::this_cpu_id()));
}

#[cfg(feature = "irq")]
pub(crate) fn next_timer_deadline() -> Option<TimeValue> {
    with_current(|runtime| runtime.next_deadline(ax_hal::percpu::this_cpu_id()))
}

fn with_current<R>(f: impl FnOnce(&mut TimerRuntime) -> R) -> R {
    let _g = crate::sync::PreemptIrqSaveGuard::new();
    // SAFETY: the guard excludes migration, IRQ/re-entry, and conflicting
    // access for the complete non-escaping mutable borrow.
    unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                TIMER_RUNTIME.with_current_mut(exclusive, f)
            })
        })
    }
    .expect("timer runtime access requires an installed CPU-local area")
}

/// Future returned by `sleep` and `sleep_until`.
///
/// A task may resume on a different CPU after any wakeup. Each CPU visited by
/// the future keeps the same timer key and shared state; stale per-CPU entries
/// therefore cannot complete the timer or retain the task's waker.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct TimerFuture {
    key: TimerKey,
    state: Arc<TimerState>,
}

impl Future for TimerFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.key.deadline <= monotonic_time() {
            self.state.cancel();
            return Poll::Ready(());
        }

        with_current(|runtime| {
            self.state
                .register(ax_hal::percpu::this_cpu_id(), cx.waker());
            runtime.ensure_registered(self.key, &self.state);
        });
        #[cfg(feature = "irq")]
        crate::timers::maybe_reprogram_timer(self.key.deadline);
        Poll::Pending
    }
}

impl Drop for TimerFuture {
    fn drop(&mut self) {
        self.state.cancel();
        with_current(|runtime| runtime.cancel(&self.key));
    }
}

/// Waits until `duration` has elapsed.
pub async fn sleep(duration: Duration) {
    sleep_until(monotonic_time() + duration).await
}

/// Waits until the monotonic `deadline` is reached.
pub async fn sleep_until(deadline: TimeValue) {
    let timer = with_current(|runtime| runtime.add(deadline, ax_hal::percpu::this_cpu_id()));
    if let Some(timer) = timer {
        timer.await;
    }
}

/// Error returned by [`timeout`] and [`timeout_at`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("task deadline elapsed")]
pub struct Elapsed(());

/// Requires a `Future` to complete before the specified duration has elapsed.
pub async fn timeout<F: IntoFuture>(
    duration: Option<Duration>,
    f: F,
) -> Result<F::Output, Elapsed> {
    timeout_at(
        duration.and_then(|x| x.checked_add(ax_hal::time::monotonic_time())),
        f,
    )
    .await
}

/// Requires a `Future` to complete before the specified monotonic deadline.
pub async fn timeout_at<F: IntoFuture>(
    deadline: Option<TimeValue>,
    f: F,
) -> Result<F::Output, Elapsed> {
    if let Some(deadline) = deadline {
        select_biased! {
            res = f.into_future().fuse() => Ok(res),
            _ = sleep_until(deadline).fuse() => Err(Elapsed(())),
        }
    } else {
        Ok(f.await)
    }
}

/// Requires a `Future` to complete before the specified wall-clock deadline.
pub async fn timeout_at_wall<F: IntoFuture>(
    deadline: Option<TimeValue>,
    f: F,
) -> Result<F::Output, Elapsed> {
    timeout_at(deadline.map(wall_deadline_to_monotonic), f).await
}

fn wall_deadline_to_monotonic(deadline: TimeValue) -> TimeValue {
    let now_wall = wall_time();
    let now_mono = monotonic_time();
    if deadline <= now_wall {
        now_mono
    } else {
        now_mono
            .checked_add(deadline - now_wall)
            .unwrap_or(TimeValue::MAX)
    }
}

#[cfg(all(test, not(target_os = "none")))]
mod tests {
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    use super::*;

    struct WakeCounter(AtomicUsize);

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn migrated_timer_ignores_stale_per_cpu_entry() {
        let key = TimerKey {
            deadline: TimeValue::ZERO,
            key: 0,
        };
        let state = Arc::new(TimerState::new(0));
        let wake_counter = Arc::new(WakeCounter(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&wake_counter));
        state.register(1, &waker);

        let mut old_runtime = TimerRuntime::new();
        old_runtime.wheel.insert(key, Arc::clone(&state));
        let mut current_runtime = TimerRuntime::new();
        current_runtime.wheel.insert(key, state);

        old_runtime.wake(0);
        assert!(old_runtime.wheel.is_empty());
        assert_eq!(wake_counter.0.load(Ordering::Relaxed), 0);

        current_runtime.wake(1);
        assert!(current_runtime.wheel.is_empty());
        assert_eq!(wake_counter.0.load(Ordering::Relaxed), 1);
    }
}
