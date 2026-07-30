//! Time management module.

use alloc::{borrow::ToOwned, collections::binary_heap::BinaryHeap, sync::Arc};
use core::{
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ax_kernel_guard::NoPreempt;
use ax_runtime::hal::time::{NANOS_PER_SEC, TimeValue, monotonic_time_nanos, wall_time};
use ax_std::os::arceos::task as scheduler;
use ax_sync::PiMutex;
use event_listener::{Event, listener};
use spin::LazyLock;
use starry_process::Pid;
use starry_signal::Signo;
use strum::FromRepr;

use crate::task::{
    future::{block_on, timeout_at_wall},
    poll_process_timer_for_alarm,
};

mod accounting;
mod alarm;
mod common;
mod itimer;
mod rttime;

pub use accounting::{CpuTimeAccounting, ProcessCpuTimeAccounting, TimerState};
pub(crate) use accounting::{CpuTimeDelta, ProcessCpuTimeSnapshot};
pub(crate) use alarm::{AlarmChange, AlarmSlot, AlarmToken};
pub use alarm::{AlarmTarget, spawn_alarm_task};
use common::time_value_from_nanos;
pub(crate) use itimer::{ITimerSetting, PendingTimerActions, SetITimerOutcome};
pub use itimer::{ITimerType, ProcessTimerManager};
pub(crate) use rttime::RttimeLimitAction;
pub use rttime::RttimeWatchdog;

include!("timer/axtest.rs");
