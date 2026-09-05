//! timerfd — kernel-side timer events delivered via a file descriptor.
//!
//! Userspace creates a timerfd via `timerfd_create(clockid, flags)`, arms it
//! with `timerfd_settime(fd, flags, new, old)`, and reads the cumulative
//! number of expirations as a `u64` via `read(fd)`. The fd is epoll-pollable
//! (becomes readable when `expire_count > 0`).
//!
//! Implementation model: each `Timerfd::new` spawns exactly one long-lived
//! background task (via `ax_task::spawn_raw`) that owns a weak reference to
//! the Timerfd. The task loops, reading the current deadline under the state
//! lock, then parks on whichever fires first: the clock-domain deadline or an
//! "arm event" poked by rearming operations / `Drop`. One task
//! per timerfd over its whole lifetime — no per-settime stack leak.
//!
//! A firing publishes one expiration and parks the task. Periodic timers are
//! advanced and rearmed on read/gettime, coalescing missed ticks without
//! repeatedly waking the task while userspace has not consumed an expiration.

use alloc::{
    borrow::{Cow, ToOwned},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::Context,
    time::Duration,
};

use ax_lazyinit::LazyLock;
use ax_runtime::hal::time::{TimeValue, monotonic_time, wall_time};
use ax_task::future::{block_on, poll_io, timeout_at, timeout_at_wall};
use axpoll::{IoEvents, PollSet, Pollable};
use event_listener::{Event, listener};
use syscalls::Errno;

use crate::{
    StarryError, StarryResult,
    file::{FileLike, IoDst, IoSrc},
    sync::Mutex,
};

/// `clockid_t` values recognized by `timerfd_create`. Kept narrow for now —
/// musl and glibc both pass `CLOCK_REALTIME` or `CLOCK_MONOTONIC`. Other
/// values return `StarryError::InvalidInput`.
pub const CLOCK_REALTIME: u32 = 0;
pub const CLOCK_MONOTONIC: u32 = 1;
pub const CLOCK_BOOTTIME: u32 = 7;
pub const CLOCK_REALTIME_ALARM: u32 = 8;
pub const CLOCK_BOOTTIME_ALARM: u32 = 9;

/// `flags` bits for `timerfd_settime`.
pub const TFD_TIMER_ABSTIME: u32 = 1;
pub const TFD_TIMER_CANCEL_ON_SET: u32 = 2;

/// Interpretation of the initial timerfd expiration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerfdSetMode {
    /// The value is an interval measured by the monotonic clock.
    Relative,
    /// The value is an absolute deadline in the timerfd's clock domain.
    Absolute,
    /// The value is an absolute deadline that is canceled by realtime changes.
    AbsoluteCancelOnSet,
}

impl TimerfdSetMode {
    fn is_absolute(self) -> bool {
        !matches!(self, Self::Relative)
    }

    fn cancel_on_set(self) -> bool {
        matches!(self, Self::AbsoluteCancelOnSet)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimerDeadline {
    Monotonic(TimeValue),
    Realtime(TimeValue),
}

impl TimerDeadline {
    fn now(self) -> TimeValue {
        match self {
            Self::Monotonic(_) => monotonic_time(),
            Self::Realtime(_) => wall_time(),
        }
    }

    fn value(self) -> TimeValue {
        match self {
            Self::Monotonic(value) | Self::Realtime(value) => value,
        }
    }

    fn remaining(self) -> Duration {
        self.value()
            .checked_sub(self.now())
            .unwrap_or(Duration::ZERO)
    }

    fn lag(self) -> Option<Duration> {
        self.now().checked_sub(self.value())
    }

    fn saturating_add(self, duration: Duration) -> Self {
        match self {
            Self::Monotonic(value) => Self::Monotonic(value.saturating_add(duration)),
            Self::Realtime(value) => Self::Realtime(value.saturating_add(duration)),
        }
    }

    fn is_realtime(self) -> bool {
        matches!(self, Self::Realtime(_))
    }
}

/// Internal, mutex-protected state of a timerfd.
#[derive(Default)]
struct State {
    /// Armed deadline, or the last fired deadline while `expired` is set.
    /// `None` means disarmed with no expiration to rearm.
    next_deadline: Option<TimerDeadline>,
    /// The task fired once and is waiting for read/gettime before rearming.
    expired: bool,
    /// Interval for periodic firing. `Duration::ZERO` means one-shot.
    interval: Duration,
    /// Whether this absolute realtime setting is registered for clock cancellation.
    cancel_on_set: bool,
    /// Whether the next read must consume a realtime clock cancellation.
    canceled: bool,
    /// When `true`, the background task should exit on its next wake.
    shutdown: bool,
}

impl State {
    /// Rearms an expired periodic timer and returns additional missed ticks.
    /// The firing itself has already contributed one tick.
    fn rearm_periodic(&mut self) -> u64 {
        if !self.expired || self.interval.is_zero() {
            return 0;
        }
        let deadline = self
            .next_deadline
            .expect("expired timer retains its deadline");
        self.expired = false;
        let Some(lag) = deadline.lag() else {
            // A backward clock step can put the old expiration in the future.
            return 0;
        };
        let interval_ns = self.interval.as_nanos();
        let remainder_ns = lag.as_nanos() % interval_ns;
        let remainder = Duration::new(
            (remainder_ns / 1_000_000_000) as u64,
            (remainder_ns % 1_000_000_000) as u32,
        );
        // Forward from now to the next boundary without multiplying a tick
        // count into a Duration; large clock steps are handled in one pass.
        self.next_deadline = Some(
            deadline
                .saturating_add(lag)
                .saturating_add(self.interval - remainder),
        );
        (lag.as_nanos() / interval_ns).min(u64::MAX as u128 - 1) as u64
    }
}

static TIMERFD_INSTANCES: LazyLock<Mutex<Vec<Weak<Timerfd>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// A timerfd. Held behind `Arc` and referenced both from the fd table and
/// from the background timer task (as a `Weak<Timerfd>`).
pub struct Timerfd {
    /// The clock domain the user passed to `timerfd_create`.
    clockid: u32,
    state: Mutex<State>,
    expire_count: AtomicU64,
    poll_rx: PollSet,
    non_blocking: AtomicBool,
    /// Pulsed when a timer is rearmed or dropped to wake the background task so it
    /// re-reads `state` and either re-arms or exits. `Arc` so the task
    /// can hold it independently of the Timerfd (allowing the Timerfd
    /// Arc to drop while the task is parked).
    arm_event: Arc<Event>,
}

impl Timerfd {
    /// Create a disarmed timerfd for the given clock. A single long-lived
    /// background task is spawned to serve all future arms of this fd.
    pub fn new(clockid: u32) -> StarryResult<Arc<Self>> {
        match clockid {
            CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME | CLOCK_REALTIME_ALARM
            | CLOCK_BOOTTIME_ALARM => {}
            _ => return Err(StarryError::InvalidInput),
        }
        let this = Arc::new(Self {
            clockid,
            state: Mutex::new(State::default()),
            expire_count: AtomicU64::new(0),
            poll_rx: PollSet::new(),
            non_blocking: AtomicBool::new(false),
            arm_event: Arc::new(Event::new()),
        });
        TIMERFD_INSTANCES.lock().push(Arc::downgrade(&this));
        // Hand a weak reference to the task so the Timerfd can be freed
        // (and the task told to exit) when userspace closes the fd.
        let weak = Arc::downgrade(&this);
        ax_task::spawn_raw(
            move || block_on(run_timer(weak)),
            "timerfd".to_owned(),
            ax_task::default_task_stack_size(),
        );
        Ok(this)
    }

    /// Arm or disarm the timer. Returns the previous `(interval, remaining)`.
    pub fn settime(
        &self,
        mode: TimerfdSetMode,
        new_value: Duration,
        new_interval: Duration,
    ) -> StarryResult<(Duration, Duration)> {
        let mut state = self.state.lock();
        state.rearm_periodic();
        let old_interval = state.interval;
        let old_remaining = state
            .next_deadline
            .map(TimerDeadline::remaining)
            .unwrap_or(Duration::ZERO);

        if new_value.is_zero() {
            state.next_deadline = None;
            state.interval = Duration::ZERO;
            state.cancel_on_set = false;
        } else {
            let deadline = if mode.is_absolute() {
                match self.clockid {
                    CLOCK_REALTIME | CLOCK_REALTIME_ALARM => TimerDeadline::Realtime(new_value),
                    _ => TimerDeadline::Monotonic(new_value),
                }
            } else {
                TimerDeadline::Monotonic(monotonic_time().saturating_add(new_value))
            };
            state.next_deadline = Some(deadline);
            state.interval = new_interval;
            state.cancel_on_set = mode.cancel_on_set() && deadline.is_realtime();
        }
        state.canceled = false;
        state.expired = false;
        // Clear any expirations that accumulated under the previous
        // setting. man timerfd_read(2) is explicit: read returns the
        // number of expirations since "the last successful read or the
        // last timerfd_settime() that reset the timer". Without this
        // reset a `settime` rearm-without-read would let the next
        // `read` return stale ticks from the old timer.
        //
        // Done under `state` so the background task, which only adds
        // expirations after re-acquiring `state` and confirming its
        // observed deadline is still current, cannot race a stale
        // fetch_add past this clear.
        self.expire_count.store(0, Ordering::Release);
        drop(state);

        // Wake the background task so it picks up the new deadline.
        self.arm_event.notify(usize::MAX);
        Ok((old_interval, old_remaining))
    }

    /// Current `(interval, remaining)`, advancing an expired periodic timer.
    pub fn gettime(&self) -> (Duration, Duration) {
        let mut state = self.state.lock();
        let rearmed = state.expired && !state.interval.is_zero();
        let extra = state.rearm_periodic();
        self.expire_count.fetch_add(extra, Ordering::AcqRel);
        let result = (
            state.interval,
            state
                .next_deadline
                .map(TimerDeadline::remaining)
                .unwrap_or(Duration::ZERO),
        );
        drop(state);
        if rearmed {
            self.arm_event.notify(usize::MAX);
        }
        result
    }

    fn take_expirations(&self) -> StarryResult<u64> {
        let mut state = self.state.lock();
        if state.canceled {
            state.canceled = false;
            if state.expired {
                state.next_deadline = None;
                state.expired = false;
            }
            // Linux consumes ticks/expired without restarting an expired
            // timer. A still-armed future deadline remains armed.
            self.expire_count.store(0, Ordering::Release);
            return Err(Errno::ECANCELED.into());
        }
        let rearmed = state.expired && !state.interval.is_zero();
        let extra = state.rearm_periodic();
        if state.expired {
            // A consumed one-shot has no future deadline.
            state.next_deadline = None;
            state.expired = false;
        }
        // Claim counts under the same lock as firing, settime and clock
        // cancellation, so concurrent readers cannot consume the same ticks.
        let count = self
            .expire_count
            .swap(0, Ordering::AcqRel)
            .saturating_add(extra);
        drop(state);
        if rearmed {
            self.arm_event.notify(usize::MAX);
        }
        if count == 0 {
            Err(StarryError::WouldBlock)
        } else {
            Ok(count)
        }
    }
}

/// Marks cancel-on-set realtime timerfds after a discontinuous clock change.
pub fn notify_realtime_clock_changed() {
    let timerfds = {
        let mut registry = TIMERFD_INSTANCES.lock();
        let mut timerfds = Vec::with_capacity(registry.len());
        registry.retain(|weak| {
            let Some(timerfd) = weak.upgrade() else {
                return false;
            };
            timerfds.push(timerfd);
            true
        });
        timerfds
    };

    for timerfd in timerfds {
        let mut state = timerfd.state.lock();
        if state.cancel_on_set {
            state.canceled = true;
            timerfd.expire_count.store(1, Ordering::Release);
            drop(state);
            timerfd.arm_event.notify(usize::MAX);
            // The cancellation marker is visible before readers are woken.
            unsafe { timerfd.poll_rx.wake(IoEvents::IN) };
        }
    }
}

impl Drop for Timerfd {
    fn drop(&mut self) {
        // Tell the background task to exit. The task holds a Weak<Timerfd>,
        // so in practice this runs only if every other ref has been released —
        // but flip the shutdown flag anyway for correctness if the last ref
        // happens to be the task's own upgrade.
        let mut state = self.state.lock();
        state.shutdown = true;
        drop(state);
        self.arm_event.notify(usize::MAX);

        // `notify_realtime_clock_changed` snapshots strong references before
        // taking any timerfd state lock, so unregistering here cannot recurse
        // into `Drop` while the registry lock is held.
        let self_ptr = core::ptr::from_ref(self);
        TIMERFD_INSTANCES
            .lock()
            .retain(|weak| weak.as_ptr() != self_ptr && weak.strong_count() != 0);
    }
}

async fn run_timer(weak: alloc::sync::Weak<Timerfd>) {
    loop {
        // Race-free arm pattern (see task/timer.rs::alarm_task):
        //   1. Upgrade, grab a standalone handle to arm_event, drop Arc.
        //   2. Register the listener.
        //   3. Re-upgrade and snapshot state. If state changed vs. anything
        //      visible before step 2, the poke was captured by the listener
        //      (or will be on next iter via `continue`).
        let arm_event = {
            let Some(tfd) = weak.upgrade() else {
                return;
            };
            tfd.arm_event.clone()
        };
        listener!(arm_event => listener);

        let (deadline, shutdown) = {
            let Some(tfd) = weak.upgrade() else {
                return;
            };
            let state = tfd.state.lock();
            (
                if state.expired {
                    None
                } else {
                    state.next_deadline
                },
                state.shutdown,
            )
        };
        if shutdown {
            return;
        }

        match deadline {
            None => {
                // Disarmed. Wait on arm_event for the next settime.
                listener.await;
            }
            Some(dl) => {
                // Race the deadline against an arm_event (new settime,
                // cancellation, or shutdown). Relative timers stay in the
                // monotonic domain; only absolute realtime timers are rebuilt
                // after a wall-clock step.
                let fired_timer = match dl {
                    TimerDeadline::Monotonic(deadline) => {
                        timeout_at(Some(deadline), listener).await.is_err()
                    }
                    TimerDeadline::Realtime(deadline) => {
                        timeout_at_wall(Some(deadline), listener).await.is_err()
                    }
                };
                if !fired_timer {
                    // State changed; loop to re-read.
                    continue;
                }

                let Some(tfd) = weak.upgrade() else {
                    return;
                };
                // Fire once. Userspace read/gettime decides whether a periodic
                // timer is rearmed; an ECANCELED read must skip that restart.
                let mut state = tfd.state.lock();
                if state.shutdown {
                    return;
                }
                if !state.expired && state.next_deadline == Some(dl) {
                    state.expired = true;
                    tfd.expire_count.fetch_add(1, Ordering::AcqRel);
                    drop(state);
                    // expire_count is published before waking readers.
                    unsafe { tfd.poll_rx.wake(IoEvents::IN) };
                }
            }
        }
    }
}

impl FileLike for Timerfd {
    fn read(&self, dst: &mut IoDst) -> StarryResult<usize> {
        if dst.remaining_mut() < core::mem::size_of::<u64>() {
            return Err(StarryError::InvalidInput);
        }
        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            let n = self.take_expirations()?;
            // Linux's timerfd_read(2): a failed read does not discard
            // expirations. Restore the claimed count on copyout failure,
            // and re-wake `poll_rx` so any reader or poller that
            // entered its wait between claiming the count and this restore
            // notices the fd is readable again.
            if let Err(e) = dst.write(&n.to_ne_bytes()) {
                self.expire_count.fetch_add(n, Ordering::AcqRel);
                // Restored expire_count is visible before re-waking readers.
                unsafe { self.poll_rx.wake(IoEvents::IN) };
                return Err(e.into());
            }
            Ok(core::mem::size_of::<u64>())
        }))
    }

    fn write(&self, _src: &mut IoSrc) -> StarryResult<usize> {
        Err(StarryError::InvalidInput)
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, non_blocking: bool) -> StarryResult {
        self.non_blocking.store(non_blocking, Ordering::Release);
        Ok(())
    }

    fn path(&self) -> Cow<'_, str> {
        "anon_inode:[timerfd]".into()
    }
}

impl Pollable for Timerfd {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, self.expire_count.load(Ordering::Acquire) > 0);
        events
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            // Registration happens from file poll task context.
            unsafe { self.poll_rx.register(context.waker(), IoEvents::IN) };
        }
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    fn unspawned_timerfd() -> Arc<Timerfd> {
        Arc::new(Timerfd {
            clockid: CLOCK_REALTIME,
            state: Mutex::new(State::default()),
            expire_count: AtomicU64::new(0),
            poll_rx: PollSet::new(),
            non_blocking: AtomicBool::new(false),
            arm_event: Arc::new(Event::new()),
        })
    }

    #[test]
    fn canceled_expiration_is_consumed_without_rearming() {
        let timerfd = unspawned_timerfd();
        {
            let mut state = timerfd.state.lock();
            state.next_deadline = Some(TimerDeadline::Realtime(Duration::from_secs(10)));
            state.interval = Duration::from_millis(10);
            state.expired = true;
            state.canceled = true;
        }
        timerfd.expire_count.store(2, Ordering::Release);

        assert!(matches!(
            timerfd.take_expirations(),
            Err(StarryError::Errno(Errno::ECANCELED))
        ));
        assert!(matches!(
            timerfd.take_expirations(),
            Err(StarryError::WouldBlock)
        ));
        let state = timerfd.state.lock();
        assert_eq!(state.next_deadline, None);
        assert!(!state.expired);
        assert!(!state.canceled);
        assert_eq!(state.interval, Duration::from_millis(10));
    }

    #[test]
    fn cancellation_read_preserves_an_unexpired_timer() {
        let timerfd = unspawned_timerfd();
        let deadline = TimerDeadline::Realtime(Duration::from_secs(600));
        {
            let mut state = timerfd.state.lock();
            state.next_deadline = Some(deadline);
            state.canceled = true;
        }
        timerfd.expire_count.store(1, Ordering::Release);

        assert!(matches!(
            timerfd.take_expirations(),
            Err(StarryError::Errno(Errno::ECANCELED))
        ));
        assert_eq!(timerfd.state.lock().next_deadline, Some(deadline));
    }

    #[test]
    fn dropping_timerfd_unregisters_clock_change_observer() {
        let timerfd = unspawned_timerfd();
        let timerfd_ptr = Arc::as_ptr(&timerfd);
        TIMERFD_INSTANCES.lock().push(Arc::downgrade(&timerfd));

        drop(timerfd);

        assert!(
            !TIMERFD_INSTANCES
                .lock()
                .iter()
                .any(|weak| weak.as_ptr() == timerfd_ptr),
            "closed timerfd remained in the realtime clock observer registry"
        );
    }
}
