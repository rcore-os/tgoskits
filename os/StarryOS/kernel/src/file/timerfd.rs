use alloc::{borrow::Cow, sync::Arc};
use core::{
    mem::size_of,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use ax_hal::time::{TimeValue, monotonic_time, wall_time};
use ax_task::future::{block_on, poll_io, sleep_until};
use axpoll::{IoEvents, PollSet, Pollable};
use linux_raw_sys::general::{
    __kernel_clockid_t, CLOCK_BOOTTIME, CLOCK_MONOTONIC, CLOCK_REALTIME, itimerspec,
};
use spin::Mutex;

use crate::{
    file::{FileLike, IoDst, IoSrc},
    time::TimeValueLike,
};

struct TimerFdState {
    clock_id: __kernel_clockid_t,
    generation: u64,
    deadline: Option<TimeValue>,
    interval: Option<TimeValue>,
    expirations: u64,
}

pub struct TimerFd {
    non_blocking: AtomicBool,
    poll_rx: PollSet,
    state: Mutex<TimerFdState>,
}

impl TimerFd {
    pub fn new(clock_id: __kernel_clockid_t) -> Arc<Self> {
        Arc::new(Self {
            non_blocking: AtomicBool::new(false),
            poll_rx: PollSet::new(),
            state: Mutex::new(TimerFdState {
                clock_id,
                generation: 0,
                deadline: None,
                interval: None,
                expirations: 0,
            }),
        })
    }

    fn now_for_clock(clock_id: __kernel_clockid_t) -> AxResult<TimeValue> {
        match clock_id as u32 {
            CLOCK_REALTIME => Ok(wall_time()),
            CLOCK_MONOTONIC | CLOCK_BOOTTIME => Ok(monotonic_time()),
            _ => Err(AxError::InvalidInput),
        }
    }

    fn to_timer_spec(
        clock_id: __kernel_clockid_t,
        deadline: Option<TimeValue>,
        interval: Option<TimeValue>,
    ) -> AxResult<itimerspec> {
        let now = Self::now_for_clock(clock_id)?;
        Ok(itimerspec {
            it_interval: timespec_or_zero(interval),
            it_value: timespec_or_zero(deadline.map(|it| it.saturating_sub(now))),
        })
    }

    pub fn get_time(&self) -> AxResult<itimerspec> {
        let state = self.state.lock();
        Self::to_timer_spec(state.clock_id, state.deadline, state.interval)
    }

    pub fn set_time(self: &Arc<Self>, flags: u32, new_value: itimerspec) -> AxResult<itimerspec> {
        let interval = new_value.it_interval.try_into_time_value()?;
        let value = new_value.it_value.try_into_time_value()?;
        let clock_id;
        let generation;
        let armed_deadline;
        let old_value;

        {
            let mut state = self.state.lock();
            clock_id = state.clock_id;
            old_value = Self::to_timer_spec(state.clock_id, state.deadline, state.interval)?;
            state.generation = state.generation.wrapping_add(1);
            generation = state.generation;
            state.interval = if interval.is_zero() {
                None
            } else {
                Some(interval)
            };
            state.deadline = if value.is_zero() {
                None
            } else if flags & linux_raw_sys::general::TFD_TIMER_ABSTIME != 0 {
                Some(value)
            } else {
                Some(Self::now_for_clock(state.clock_id)? + value)
            };
            armed_deadline = state.deadline;
        }

        if let Some(deadline) = armed_deadline {
            self.start_worker(generation, clock_id, deadline);
        }

        Ok(old_value)
    }

    fn start_worker(
        self: &Arc<Self>,
        generation: u64,
        clock_id: __kernel_clockid_t,
        mut deadline: TimeValue,
    ) {
        let timerfd = self.clone();
        ax_task::spawn_with_name(
            move || {
                ax_task::future::block_on(async move {
                    loop {
                        sleep_until(deadline).await;

                        let mut next_deadline = None;
                        {
                            let mut state = timerfd.state.lock();
                            if state.generation != generation || state.deadline.is_none() {
                                return;
                            }

                            let now = match Self::now_for_clock(clock_id) {
                                Ok(now) => now,
                                Err(_) => return,
                            };
                            let Some(current_deadline) = state.deadline else {
                                return;
                            };

                            if now < current_deadline {
                                next_deadline = Some(current_deadline);
                            } else {
                                state.expirations = state.expirations.saturating_add(1);
                                if let Some(interval) = state.interval {
                                    let mut next = current_deadline + interval;
                                    while next <= now {
                                        next += interval;
                                        state.expirations = state.expirations.saturating_add(1);
                                    }
                                    state.deadline = Some(next);
                                    next_deadline = Some(next);
                                } else {
                                    state.deadline = None;
                                }
                            }
                        }

                        timerfd.poll_rx.wake();

                        let Some(next) = next_deadline else {
                            return;
                        };
                        deadline = next;
                    }
                })
            },
            "timerfd".into(),
        );
    }
}

impl FileLike for TimerFd {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        if dst.remaining_mut() < size_of::<u64>() {
            return Err(AxError::InvalidInput);
        }

        block_on(poll_io(self, IoEvents::IN, self.nonblocking(), || {
            let expirations = {
                let mut state = self.state.lock();
                if state.expirations == 0 {
                    return Err(AxError::WouldBlock);
                }
                let expirations = state.expirations;
                state.expirations = 0;
                expirations
            };
            dst.write(&expirations.to_ne_bytes())?;
            Ok(size_of::<u64>())
        }))
    }

    fn write(&self, _src: &mut IoSrc) -> AxResult<usize> {
        Err(AxError::BadFileDescriptor)
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

impl Pollable for TimerFd {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::empty();
        events.set(IoEvents::IN, self.state.lock().expirations > 0);
        events
    }

    fn register(&self, context: &mut core::task::Context<'_>, events: IoEvents) {
        if events.contains(IoEvents::IN) {
            self.poll_rx.register(context.waker());
        }
    }
}

fn timespec_or_zero(value: Option<Duration>) -> linux_raw_sys::general::timespec {
    let value = value.unwrap_or_default();
    linux_raw_sys::general::timespec::from_time_value(value)
}
