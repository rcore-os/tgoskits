use alloc::collections::BTreeMap;
use core::{
    pin::Pin,
    task::{Context, Poll, Waker},
    time::Duration,
};

use ax_hal::time::{TimeValue, monotonic_time, wall_time};
use futures_util::{FutureExt, select_biased};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TimerKey {
    deadline: TimeValue,
    key: u64,
}

pub(crate) struct TimerRuntime {
    key: u64,
    wheel: BTreeMap<TimerKey, Waker>,
    // Once IRQ processing publishes the due head to the timer worker, the
    // logical queue must stop advertising it to the physical clockevent.
    // The worker owns clearing this state after its bounded drain pass.
    #[cfg(feature = "irq")]
    due_work_published: bool,
}

impl TimerRuntime {
    pub(crate) const fn new() -> Self {
        TimerRuntime {
            key: 0,
            wheel: BTreeMap::new(),
            #[cfg(feature = "irq")]
            due_work_published: false,
        }
    }

    pub(crate) fn add(&mut self, deadline: TimeValue) -> Option<TimerKey> {
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

    pub(crate) fn poll(&mut self, key: &TimerKey, cx: &mut Context<'_>) -> Poll<()> {
        if let Some(w) = self.wheel.get_mut(key) {
            *w = cx.waker().clone();
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }

    pub(crate) fn cancel(&mut self, key: &TimerKey) {
        self.wheel.remove(key);
    }

    #[cfg(feature = "irq")]
    pub(crate) fn next_deadline(&self) -> Option<TimeValue> {
        if self.due_work_published {
            return None;
        }
        self.wheel.keys().next().map(|key| key.deadline)
    }

    #[cfg(feature = "irq")]
    pub(crate) fn publish_due_work(&mut self, now: TimeValue) -> bool {
        self.due_work_published |= self
            .wheel
            .keys()
            .next()
            .is_some_and(|key| key.deadline <= now);
        self.due_work_published
    }

    #[cfg(feature = "irq")]
    pub(crate) fn finish_due_work(&mut self, now: TimeValue) -> bool {
        self.due_work_published = self
            .wheel
            .keys()
            .next()
            .is_some_and(|key| key.deadline <= now);
        self.due_work_published
    }

    #[cfg(feature = "irq")]
    pub(crate) fn expire_one(&mut self, now: TimeValue) -> Option<Waker> {
        let key = self
            .wheel
            .first_key_value()
            .and_then(|(key, _)| (key.deadline <= now).then_some(*key))?;
        self.wheel.remove(&key)
    }
}

#[cfg(not(feature = "irq"))]
percpu_static! {
    FUTURE_TIMER_RUNTIME: TimerRuntime = TimerRuntime::new(),
}

#[cfg(not(feature = "irq"))]
fn with_current_timer_runtime<R>(f: impl FnOnce(&mut TimerRuntime) -> R) -> R {
    let _guard = crate::sync::PreemptIrqSaveGuard::new();
    // SAFETY: the guard prevents migration and IRQ/re-entry for the complete
    // non-escaping mutable borrow.
    unsafe {
        ax_hal::percpu::with_cpu_pin(|pin| {
            ax_hal::percpu::with_exclusive_cpu(pin, |exclusive| {
                FUTURE_TIMER_RUNTIME.with_current_mut(exclusive, f)
            })
        })
    }
    .expect("future timer access requires an installed CPU-local area")
}

#[cfg(feature = "irq")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FutureTimerHandle {
    owner_cpu: usize,
    key: TimerKey,
}

#[cfg(feature = "irq")]
impl FutureTimerHandle {
    pub(crate) const fn new(owner_cpu: usize, key: TimerKey) -> Self {
        Self { owner_cpu, key }
    }

    pub(crate) const fn owner_cpu(self) -> usize {
        self.owner_cpu
    }

    pub(crate) const fn key(self) -> TimerKey {
        self.key
    }

    #[cfg(test)]
    const fn new_for_test(owner_cpu: usize, key: TimerKey) -> Self {
        Self::new(owner_cpu, key)
    }
}

/// Future returned by `sleep` and `sleep_until`.
#[must_use = "futures do nothing unless you `.await` or poll them"]
#[cfg(feature = "irq")]
pub struct TimerFuture(FutureTimerHandle);

#[cfg(feature = "irq")]
impl Future for TimerFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        crate::timers::poll_future_timer(self.0, cx)
    }
}

#[cfg(feature = "irq")]
impl Drop for TimerFuture {
    fn drop(&mut self) {
        crate::timers::cancel_future_timer(self.0);
    }
}

/// Future returned by `sleep` and `sleep_until` when no timer IRQ is available.
#[must_use = "futures do nothing unless you `.await` or poll them"]
#[cfg(not(feature = "irq"))]
pub struct TimerFuture(TimerKey);

#[cfg(not(feature = "irq"))]
impl Future for TimerFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        with_current_timer_runtime(|runtime| runtime.poll(&self.0, cx))
    }
}

#[cfg(not(feature = "irq"))]
impl Drop for TimerFuture {
    fn drop(&mut self) {
        with_current_timer_runtime(|runtime| runtime.cancel(&self.0));
    }
}

/// Waits until `duration` has elapsed.
pub async fn sleep(duration: Duration) {
    sleep_until(monotonic_time() + duration).await
}

/// Waits until the monotonic `deadline` is reached.
pub async fn sleep_until(deadline: TimeValue) {
    #[cfg(feature = "irq")]
    if let Some(handle) = crate::timers::register_future_timer(deadline) {
        TimerFuture(handle).await;
    }

    #[cfg(not(feature = "irq"))]
    if let Some(key) = with_current_timer_runtime(|runtime| runtime.add(deadline)) {
        TimerFuture(key).await;
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

#[cfg(all(test, feature = "irq"))]
mod timer_regression_tests {
    use super::*;

    fn poll_registered_timer_for_test(
        runtimes: [&mut TimerRuntime; 2],
        _current_cpu: usize,
        handle: &FutureTimerHandle,
        context: &mut Context<'_>,
    ) -> Poll<()> {
        runtimes[handle.owner_cpu()].poll(&handle.key(), context)
    }

    fn cancel_registered_timer_for_test(
        runtimes: [&mut TimerRuntime; 2],
        _current_cpu: usize,
        handle: &FutureTimerHandle,
    ) {
        runtimes[handle.owner_cpu()].cancel(&handle.key());
    }

    #[test]
    fn future_timer_poll_uses_the_registration_cpu_after_migration() {
        let deadline = monotonic_time() + Duration::from_secs(60);
        let mut owner = TimerRuntime::new();
        let mut current = TimerRuntime::new();
        let key = owner.add(deadline).expect("future timer must be pending");
        let handle = FutureTimerHandle::new_for_test(0, key);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);

        let result =
            poll_registered_timer_for_test([&mut owner, &mut current], 1, &handle, &mut context);

        assert_eq!(result, Poll::Pending);
        assert!(owner.wheel.contains_key(&key));
        assert!(current.wheel.is_empty());
    }

    #[test]
    fn future_timer_drop_cancels_the_registration_cpu_after_migration() {
        let deadline = monotonic_time() + Duration::from_secs(60);
        let mut owner = TimerRuntime::new();
        let mut current = TimerRuntime::new();
        let key = owner.add(deadline).expect("future timer must be pending");
        let handle = FutureTimerHandle::new_for_test(0, key);

        cancel_registered_timer_for_test([&mut owner, &mut current], 1, &handle);

        assert!(owner.wheel.is_empty());
        assert!(current.wheel.is_empty());
    }

    #[test]
    fn due_future_work_is_not_republished_as_a_clockevent_deadline() {
        let mut runtime = TimerRuntime::new();
        let deadline = monotonic_time() + Duration::from_secs(60);
        runtime.add(deadline).expect("future timer must be pending");

        assert!(runtime.publish_due_work(deadline));
        assert_eq!(runtime.next_deadline(), None);
    }

    #[test]
    fn future_deadline_is_republished_after_the_due_pass_finishes() {
        let mut runtime = TimerRuntime::new();
        let deadline = monotonic_time() + Duration::from_secs(60);
        let later_deadline = deadline + Duration::from_secs(1);
        runtime.add(deadline).expect("future timer must be pending");
        runtime
            .add(later_deadline)
            .expect("later future timer must be pending");

        assert!(runtime.publish_due_work(deadline));
        assert!(runtime.expire_one(deadline).is_some());
        assert!(!runtime.finish_due_work(deadline));
        assert_eq!(runtime.next_deadline(), Some(later_deadline));
    }
}
