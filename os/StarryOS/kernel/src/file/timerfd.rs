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
//! lock, then parks on whichever fires first: the deadline (via
//! `timeout_at_wall`) or an "arm event" poked by `settime` / `Drop`. One task
//! per timerfd over its whole lifetime — no per-settime stack leak.
//!
//! Missed-tick coalescing: if the scheduler delays the task by N intervals,
//! `read` returns the full count (Linux semantics).
//!
//! Caveats vs. Linux:
//!   - Clock stepping doesn't exist, so `TFD_TIMER_CANCEL_ON_SET` is a no-op.

use alloc::{
    borrow::{Cow, ToOwned},
    sync::Arc,
};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::Context,
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_runtime::hal::time::{TimeValue, monotonic_time, wall_time};
use ax_sync::Mutex;
use ax_task::future::{block_on, poll_io, timeout_at_wall};
use axpoll::{IoEvents, PollSet, Pollable};
use event_listener::{Event, listener};

use crate::file::{FileLike, IoDst, IoSrc};

/// `clockid_t` values recognized by `timerfd_create`. Kept narrow for now —
/// musl and glibc both pass `CLOCK_REALTIME` or `CLOCK_MONOTONIC`. Other
/// values return `AxError::InvalidInput`.
pub const CLOCK_REALTIME: u32 = 0;
pub const CLOCK_MONOTONIC: u32 = 1;
pub const CLOCK_BOOTTIME: u32 = 7;
pub const CLOCK_REALTIME_ALARM: u32 = 8;
pub const CLOCK_BOOTTIME_ALARM: u32 = 9;

/// `flags` bits for `timerfd_settime`.
pub const TFD_TIMER_ABSTIME: u32 = 1;
pub const TFD_TIMER_CANCEL_ON_SET: u32 = 2;

/// Internal, mutex-protected state of a timerfd.
#[derive(Default)]
struct State {
    /// Time of the next expiration in absolute wall time. `None` when disarmed.
    next_deadline: Option<TimeValue>,
    /// Interval for periodic firing. `Duration::ZERO` means one-shot.
    interval: Duration,
    /// When `true`, the background task should exit on its next wake.
    shutdown: bool,
}

/// A timerfd. Held behind `Arc` and referenced both from the fd table and
/// from the background timer task (as a `Weak<Timerfd>`).
pub struct Timerfd {
    /// The clock domain the user passed to `timerfd_create`. Used by
    /// `settime(TFD_TIMER_ABSTIME)` to translate a user-supplied
    /// absolute deadline (which is always in this domain) into the
    /// internal wall-time domain before arming the monotonic timer wheel.
    clockid: u32,
    state: Mutex<State>,
    expire_count: AtomicU64,
    poll_rx: PollSet,
    non_blocking: AtomicBool,
    /// Pulsed by `settime` / `Drop` to wake the background task so it
    /// re-reads `state` and either re-arms or exits. `Arc` so the task
    /// can hold it independently of the Timerfd (allowing the Timerfd
    /// Arc to drop while the task is parked).
    arm_event: Arc<Event>,
}

impl Timerfd {
    /// Create a disarmed timerfd for the given clock. A single long-lived
    /// background task is spawned to serve all future arms of this fd.
    pub fn new(clockid: u32) -> AxResult<Arc<Self>> {
        match clockid {
            CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME | CLOCK_REALTIME_ALARM
            | CLOCK_BOOTTIME_ALARM => {}
            _ => return Err(AxError::InvalidInput),
        }
        let this = Arc::new(Self {
            clockid,
            state: Mutex::new(State::default()),
            expire_count: AtomicU64::new(0),
            poll_rx: PollSet::new(),
            non_blocking: AtomicBool::new(false),
            arm_event: Arc::new(Event::new()),
        });
        // Hand a weak reference to the task so the Timerfd can be freed
        // (and the task told to exit) when userspace closes the fd.
        let weak = Arc::downgrade(&this);
        ax_task::spawn_raw(
            move || block_on(run_timer(weak)),
            "timerfd".to_owned(),
            ax_config::TASK_STACK_SIZE,
        );
        Ok(this)
    }

    /// Arm or disarm the timer. Returns the previous `(interval, remaining)`.
    pub fn settime(
        &self,
        abstime: bool,
        new_value: Duration,
        new_interval: Duration,
    ) -> AxResult<(Duration, Duration)> {
        let now = wall_time();

        let mut state = self.state.lock();
        let old_interval = state.interval;
        let old_remaining = state
            .next_deadline
            .map(|dl| dl.checked_sub(now).unwrap_or(Duration::ZERO))
            .unwrap_or(Duration::ZERO);

        if new_value.is_zero() {
            state.next_deadline = None;
            state.interval = Duration::ZERO;
        } else {
            let deadline = if abstime {
                // User passed an absolute deadline in `self.clockid`'s
                // domain. CLOCK_REALTIME already uses the same epoch
                // as the timer wheel's wall_time, so the user value
                // is the wall deadline directly. CLOCK_MONOTONIC /
                // CLOCK_BOOTTIME are measured since boot; convert to
                // wall_time by adding the same offset (now - monotonic
                // now) that the kernel applies elsewhere. Without
                // this, a `clock_gettime(CLOCK_MONOTONIC) + 100ms`
                // deadline would be interpreted as a wall timestamp
                // and almost always fire immediately.
                let user_abs = TimeValue::from_secs(new_value.as_secs())
                    + Duration::from_nanos(new_value.subsec_nanos() as u64);
                match self.clockid {
                    CLOCK_REALTIME | CLOCK_REALTIME_ALARM => user_abs,
                    _ => {
                        let mono = monotonic_time();
                        let wall_minus_mono = now.checked_sub(mono).unwrap_or(Duration::ZERO);
                        user_abs.checked_add(wall_minus_mono).unwrap_or(user_abs)
                    }
                }
            } else {
                now + new_value
            };
            state.next_deadline = Some(deadline);
            state.interval = new_interval;
        }
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

    /// Current `(interval, remaining)`. `remaining == 0` iff disarmed.
    pub fn gettime(&self) -> (Duration, Duration) {
        let state = self.state.lock();
        let interval = state.interval;
        let remaining = match state.next_deadline {
            None => Duration::ZERO,
            Some(dl) => {
                let now = wall_time();
                dl.checked_sub(now).unwrap_or(Duration::ZERO)
            }
        };
        (interval, remaining)
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

        let (deadline, interval, shutdown) = {
            let Some(tfd) = weak.upgrade() else {
                return;
            };
            let state = tfd.state.lock();
            (state.next_deadline, state.interval, state.shutdown)
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
                // Race the wall-clock deadline against an arm_event (new
                // settime or shutdown). `timeout_at_wall` returns
                // Err(Elapsed) on deadline, Ok(()) if the listener fires first.
                let fired_timer = timeout_at_wall(Some(dl), listener).await.is_err();
                if !fired_timer {
                    // State changed; loop to re-read.
                    continue;
                }

                // Timer fired. Re-upgrade, compute missed-tick count,
                // advance deadline by N intervals, publish to state.
                let Some(tfd) = weak.upgrade() else {
                    return;
                };
                let now = wall_time();

                let mut expirations: u64 = 1;
                let mut next_deadline = dl;
                if !interval.is_zero() {
                    // Missed-tick coalescing: count every interval that
                    // fully elapsed past `dl`. Clamp at u32::MAX ticks so
                    // `Duration::*` multiplication cannot silently
                    // truncate; u32::MAX ticks at a 1 ns interval is still
                    // ~4 seconds of lag, which is more than any real
                    // scheduler delay we need to represent faithfully.
                    if let Some(lag) = now.checked_sub(dl) {
                        let extra_ticks = lag.as_nanos() / interval.as_nanos().max(1);
                        let extra = core::cmp::min(extra_ticks, u32::MAX as u128 - 1) as u32;
                        expirations += extra as u64;
                        // saturating_mul avoids panic on pathological
                        // (interval, extra) pairs.
                        let advance = interval.saturating_mul(extra + 1);
                        next_deadline += advance;
                    }
                }

                // Publish next deadline (or clear for one-shot) AND add
                // the expirations under the same state lock. If the
                // current next_deadline no longer matches the one we
                // just fired, someone re-armed (or disarmed) the timer
                // while we were firing — those expirations belong to
                // the now-gone timer setting, so drop them on the
                // floor. settime clears expire_count under the same
                // lock, so once we observe a stale deadline here the
                // count has already been cleared and we must not
                // re-add to it.
                let mut state = tfd.state.lock();
                if state.shutdown {
                    return;
                }
                if state.next_deadline == Some(dl) {
                    tfd.expire_count.fetch_add(expirations, Ordering::AcqRel);
                    if interval.is_zero() {
                        state.next_deadline = None;
                    } else {
                        state.next_deadline = Some(next_deadline);
                    }
                    drop(state);
                    // expire_count is published before waking readers.
                    unsafe { tfd.poll_rx.wake(IoEvents::IN) };
                }
            }
        }
    }
}

impl FileLike for Timerfd {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        if dst.remaining_mut() < core::mem::size_of::<u64>() {
            return Err(AxError::InvalidInput);
        }
        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            // Race-free read: atomically claim the entire `expire_count`
            // snapshot via CAS so concurrent readers can't both observe
            // and copy the same ticks. Linux's `timerfd_read(2)` holds
            // the timerfd spinlock across the load + clear; we get the
            // same single-consumer guarantee from the CAS loop. A
            // simultaneous `fetch_add` from the timer task raises the
            // count past `n`, the CAS fails, and we re-snapshot before
            // copying — so newly-arrived ticks aren't dropped either.
            let n = loop {
                let observed = self.expire_count.load(Ordering::Acquire);
                if observed == 0 {
                    return Err(AxError::WouldBlock);
                }
                if self
                    .expire_count
                    .compare_exchange(observed, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break observed;
                }
            };
            // Linux's timerfd_read(2): a failed read does not discard
            // expirations. Restore the claimed count on copyout failure,
            // and re-wake `poll_rx` so any reader or poller that
            // entered its wait between our CAS-to-zero and this restore
            // notices the fd is readable again.
            if let Err(e) = dst.write(&n.to_ne_bytes()) {
                self.expire_count.fetch_add(n, Ordering::AcqRel);
                // Restored expire_count is visible before re-waking readers.
                unsafe { self.poll_rx.wake(IoEvents::IN) };
                return Err(e);
            }
            Ok(core::mem::size_of::<u64>())
        }))
    }

    fn write(&self, _src: &mut IoSrc) -> AxResult<usize> {
        Err(AxError::InvalidInput)
    }

    fn nonblocking(&self) -> bool {
        self.non_blocking.load(Ordering::Acquire)
    }

    fn set_nonblocking(&self, non_blocking: bool) -> AxResult {
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
