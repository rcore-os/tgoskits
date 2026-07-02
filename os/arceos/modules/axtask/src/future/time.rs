use alloc::string::String;
use core::{
    fmt,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use ax_errno::AxError;
use ax_hal::time::{TimeValue, monotonic_time, wall_time};
use ax_kspin::SpinNoIrq;
use bare_task::{TimerKey, TimerRuntimeCore};
use futures_util::{FutureExt, select_biased};

static TIMER_RUNTIME: SpinNoIrq<TimerRuntimeCore> = SpinNoIrq::new(TimerRuntimeCore::new());
static TIMER_SIGNAL: crate::HardIrqSignal = crate::HardIrqSignal::new();
static TIMER_SERVICE_SPAWNED: AtomicBool = AtomicBool::new(false);
static TIMER_SERVICE_PENDING: AtomicBool = AtomicBool::new(false);
static NEXT_DEADLINE_NANOS: AtomicU64 = AtomicU64::new(0);

#[allow(dead_code)]
pub(crate) fn check_timer_events() {
    check_timer_events_at(deadline_to_nanos(monotonic_time()));
}

fn check_timer_events_at(now_nanos: u64) {
    let deadline = NEXT_DEADLINE_NANOS.load(Ordering::Acquire);
    if deadline != 0 && deadline <= now_nanos && !TIMER_SERVICE_PENDING.swap(true, Ordering::AcqRel)
    {
        TIMER_SIGNAL.notify_irq();
    }
}

pub(crate) fn next_timer_deadline() -> Option<TimeValue> {
    if TIMER_SERVICE_PENDING.load(Ordering::Acquire) {
        return None;
    }
    let deadline = NEXT_DEADLINE_NANOS.load(Ordering::Acquire);
    (deadline != 0).then(|| TimeValue::from_nanos(deadline))
}

fn with_runtime<R>(f: impl FnOnce(&mut TimerRuntimeCore) -> R) -> R {
    f(&mut TIMER_RUNTIME.lock())
}

fn deadline_to_nanos(deadline: TimeValue) -> u64 {
    deadline.as_nanos().min(u64::MAX as u128) as u64
}

fn update_next_deadline(runtime: &TimerRuntimeCore) {
    let deadline = runtime.next_deadline_nanos().unwrap_or(0);
    NEXT_DEADLINE_NANOS.store(deadline, Ordering::Release);
}

fn ensure_timer_service_spawned() {
    if TIMER_SERVICE_SPAWNED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    crate::spawn_raw(
        || loop {
            TIMER_SIGNAL.wait();
            drain_expired_timers();
        },
        String::from("future-timer"),
        crate::default_task_stack_size(),
    );
}

fn drain_expired_timers() {
    loop {
        TIMER_SERVICE_PENDING.store(false, Ordering::Release);
        let now = monotonic_time();
        let now_nanos = deadline_to_nanos(now);
        let expired = with_runtime(|runtime| {
            let expired = runtime.take_expired(now_nanos);
            update_next_deadline(runtime);
            expired
        });
        if expired.is_empty() {
            break;
        }
        for waker in expired {
            waker.wake();
        }
    }
    if let Some(deadline) = next_timer_deadline() {
        if deadline <= monotonic_time() {
            TIMER_SIGNAL.notify();
        } else {
            crate::timers::maybe_reprogram_timer(deadline);
        }
    }
}

/// Future returned by `sleep` and `sleep_until`.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct TimerFuture(TimerKey);

impl Future for TimerFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        with_runtime(|r| r.poll(&self.0, cx.waker()))
    }
}

impl Drop for TimerFuture {
    fn drop(&mut self) {
        with_runtime(|runtime| {
            runtime.cancel(&self.0);
            update_next_deadline(runtime);
        });
    }
}

/// Waits until `duration` has elapsed.
pub async fn sleep(duration: Duration) {
    sleep_until(monotonic_time() + duration).await
}

/// Waits until the monotonic `deadline` is reached.
pub async fn sleep_until(deadline: TimeValue) {
    ensure_timer_service_spawned();
    let deadline_nanos = deadline_to_nanos(deadline);
    let now_nanos = deadline_to_nanos(monotonic_time());
    let key = with_runtime(|runtime| {
        let key = runtime.add(deadline_nanos, now_nanos);
        update_next_deadline(runtime);
        key
    });
    if let Some(key) = key {
        crate::timers::maybe_reprogram_timer(deadline);
        TimerFuture(key).await;
    }
}

/// Error returned by [`timeout`] and [`timeout_at`].
#[derive(Debug, PartialEq, Eq)]
pub struct Elapsed(());

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "deadline elapsed")
    }
}

impl core::error::Error for Elapsed {}

impl From<Elapsed> for AxError {
    fn from(_: Elapsed) -> Self {
        AxError::TimedOut
    }
}

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

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, task::Wake};
    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        task::Waker,
    };

    use super::*;

    struct WakeCounter(AtomicUsize);

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn timer_runtime_takes_expired_wakers_without_waking_under_lock() {
        let mut runtime = TimerRuntimeCore::new();
        let counter = Arc::new(WakeCounter(AtomicUsize::new(0)));
        let waker = Waker::from(counter.clone());

        let deadline = monotonic_time() + Duration::from_millis(1);
        let deadline_nanos = deadline_to_nanos(deadline);
        let key = runtime
            .add(deadline_nanos, deadline_to_nanos(monotonic_time()))
            .expect("future timer should be armed");
        assert!(runtime.poll(&key, &waker).is_pending());

        let expired = runtime.take_expired(deadline_to_nanos(deadline + Duration::from_millis(1)));
        assert_eq!(counter.0.load(Ordering::Acquire), 0);
        assert_eq!(expired.len(), 1);

        for waker in expired {
            waker.wake();
        }
        assert_eq!(counter.0.load(Ordering::Acquire), 1);
    }

    #[test]
    fn check_timer_events_marks_service_pending_without_losing_deadline() {
        let expired = 1;
        NEXT_DEADLINE_NANOS.store(expired, Ordering::Release);
        TIMER_SERVICE_PENDING.store(false, Ordering::Release);

        check_timer_events_at(expired);

        assert_eq!(NEXT_DEADLINE_NANOS.load(Ordering::Acquire), expired);
        assert!(TIMER_SERVICE_PENDING.load(Ordering::Acquire));
        let _ = TIMER_SIGNAL.drain();
        TIMER_SERVICE_PENDING.store(false, Ordering::Release);
    }
}
