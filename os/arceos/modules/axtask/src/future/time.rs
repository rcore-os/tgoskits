use alloc::collections::BTreeMap;
use core::{
    fmt,
    pin::Pin,
    task::{Context, Poll, Waker},
    time::Duration,
};

use ax_errno::AxError;
use ax_hal::time::{TimeValue, monotonic_time, wall_time};
use futures_util::{FutureExt, select_biased};

use crate::{WaitChannel, WaitChannelGuard};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TimerKey {
    deadline: TimeValue,
    key: u64,
}

struct TimerRuntime {
    key: u64,
    wheel: BTreeMap<TimerKey, Waker>,
}

impl TimerRuntime {
    const fn new() -> Self {
        TimerRuntime {
            key: 0,
            wheel: BTreeMap::new(),
        }
    }

    fn add(&mut self, deadline: TimeValue) -> Option<TimerKey> {
        if deadline <= monotonic_time() {
            return None;
        }

        let key = TimerKey {
            deadline,
            key: self.key,
        };
        self.wheel.insert(key, Waker::noop().clone());
        self.key += 1;

        Some(key)
    }

    fn poll(&mut self, key: &TimerKey, cx: &mut Context<'_>) -> Poll<()> {
        if let Some(w) = self.wheel.get_mut(key) {
            *w = cx.waker().clone();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }

    fn cancel(&mut self, key: &TimerKey) {
        self.wheel.remove(key);
    }

    #[cfg(feature = "irq")]
    fn next_deadline(&self) -> Option<TimeValue> {
        self.wheel.keys().next().map(|key| key.deadline)
    }

    fn wake(&mut self) {
        if self.wheel.is_empty() {
            return;
        }

        let now = monotonic_time();

        let pending = self.wheel.split_off(&TimerKey {
            deadline: now,
            key: u64::MAX,
        });

        let expired = core::mem::replace(&mut self.wheel, pending);
        for (_, w) in expired {
            w.wake();
        }
    }
}

percpu_static! {
    TIMER_RUNTIME: TimerRuntime = TimerRuntime::new(),
}

#[allow(dead_code)]
pub(crate) fn check_timer_events() {
    // SAFETY: only called in timer::check_events
    unsafe { TIMER_RUNTIME.current_ref_mut_raw() }.wake();
}

#[cfg(feature = "irq")]
pub(crate) fn next_timer_deadline() -> Option<TimeValue> {
    with_current(|r| r.next_deadline())
}

fn with_current<R>(f: impl FnOnce(&mut TimerRuntime) -> R) -> R {
    // FIXME: optimize `ax-percpu` crate! should disable irq and provide more apis
    let _g = ax_kernel_guard::NoPreemptIrqSave::new();
    f(unsafe { TIMER_RUNTIME.current_ref_mut_raw() })
}

/// Future returned by `sleep` and `sleep_until`.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct TimerFuture {
    key: TimerKey,
    channel: Option<WaitChannel>,
    wchan_guard: Option<WaitChannelGuard>,
}

impl Future for TimerFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match with_current(|r| r.poll(&this.key, cx)) {
            Poll::Ready(()) => Poll::Ready(()),
            Poll::Pending => {
                if let Some(channel) = this.channel {
                    this.wchan_guard
                        .get_or_insert_with(|| WaitChannelGuard::set(channel));
                }
                Poll::Pending
            }
        }
    }
}

impl Drop for TimerFuture {
    fn drop(&mut self) {
        with_current(|r| r.cancel(&self.key));
    }
}

/// Waits until `duration` has elapsed.
pub async fn sleep(duration: Duration) {
    sleep_until(monotonic_time() + duration).await
}

/// Waits until the monotonic `deadline` is reached.
pub async fn sleep_until(deadline: TimeValue) {
    sleep_until_with_wchan(deadline, WaitChannel::ScheduleTimeout).await
}

/// Waits until `deadline` is reached, reporting the supplied wait-channel
/// label only while the timer future is actually pending.
pub async fn sleep_until_with_wchan(deadline: TimeValue, channel: WaitChannel) {
    sleep_until_with_optional_wchan(deadline, Some(channel)).await
}

async fn sleep_until_without_wchan(deadline: TimeValue) {
    sleep_until_with_optional_wchan(deadline, None).await
}

async fn sleep_until_with_optional_wchan(deadline: TimeValue, channel: Option<WaitChannel>) {
    let key = with_current(|r| r.add(deadline));
    if let Some(key) = key {
        #[cfg(feature = "irq")]
        crate::timers::maybe_reprogram_timer(deadline);
        TimerFuture {
            key,
            channel,
            wchan_guard: None,
        }
        .await;
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
            _ = sleep_until_without_wchan(deadline).fuse() => Err(Elapsed(())),
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
