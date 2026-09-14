//! Time management module.

use alloc::{
    borrow::ToOwned,
    sync::{Arc, Weak},
};
use core::{
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
    time::Duration,
};

use ax_lazyinit::LazyLock;
use ax_runtime::hal::time::{NANOS_PER_SEC, TimeValue, monotonic_time_nanos};
use ax_std::os::arceos::{task as scheduler, task::sync::WaitQueue};
use starry_signal::Signo;
use strum::FromRepr;

use super::PidIdentity;
use crate::{
    sync::{Mutex, RawSpinLock},
    task::poll_process_timer_for_alarm,
};

mod accounting;
mod alarm;
mod common;
mod itimer;
mod rttime;

pub use accounting::{CpuTimeAccounting, ProcessCpuTimeAccounting};
pub(crate) use accounting::{CpuTimeDelta, ProcessCpuTimeSnapshot};
pub(crate) use alarm::{AlarmChange, AlarmSlot, AlarmToken};
pub use alarm::{AlarmTarget, notify_realtime_clock_changed, spawn_alarm_task};
use common::time_value_from_nanos;
pub(crate) use itimer::{ITimerSetting, PendingTimerActions, SetITimerOutcome};
pub use itimer::{ITimerType, ProcessTimerManager};
pub(crate) use rttime::RttimeLimitAction;
pub use rttime::RttimeWatchdog;

#[cfg(all(test, axtest))]
mod axtest;
