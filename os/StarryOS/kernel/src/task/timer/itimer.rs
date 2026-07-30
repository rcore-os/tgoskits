use super::*;

const MAX_ITIMER_NANOS: u64 = i64::MAX as u64;

/// The type of interval timer.
#[repr(i32)]
#[allow(non_camel_case_types)]
#[derive(Eq, PartialEq, Debug, Clone, Copy, FromRepr)]
pub enum ITimerType {
    /// 统计系统实际运行时间
    Real    = 0,
    /// 统计用户态运行时间
    Virtual = 1,
    /// 统计进程的所有用户态/内核态运行时间
    Prof    = 2,
}

impl ITimerType {
    /// Returns the signal number associated with this timer type.
    pub fn signo(&self) -> Signo {
        match self {
            ITimerType::Real => Signo::SIGALRM,
            ITimerType::Virtual => Signo::SIGVTALRM,
            ITimerType::Prof => Signo::SIGPROF,
        }
    }

    fn clock_now_ns(self, snapshot: ProcessCpuTimeSnapshot) -> u64 {
        match self {
            Self::Real => snapshot.sampled_at_ns,
            Self::Virtual => snapshot.user_ns,
            Self::Prof => snapshot.user_ns.saturating_add(snapshot.system_ns),
        }
    }
}

fn itimer_alarm_delay(ty: ITimerType, remaining_ns: u64) -> Duration {
    let divisor = match ty {
        ITimerType::Real => 1_u64,
        // Process CPU time may advance concurrently on every configured CPU.
        // Waking conservatively early keeps the task-context worker from
        // delivering a CPU timer late without putting POSIX timer callbacks in
        // the hard-IRQ path.
        ITimerType::Virtual | ITimerType::Prof => ax_runtime::CPU_CAPACITY.max(1) as u64,
    };
    Duration::from_nanos(remaining_ns.div_ceil(divisor).max(1))
}

#[derive(Clone, Copy)]
pub(crate) struct ITimerSetting {
    interval_ns: u64,
    remaining_ns: u64,
}

impl ITimerSetting {
    pub(crate) fn new(interval: TimeValue, remaining: TimeValue) -> Self {
        Self {
            interval_ns: bounded_itimer_nanos(interval),
            remaining_ns: bounded_itimer_nanos(remaining),
        }
    }
}

struct ITimer {
    interval_ns: u64,
    deadline_ns: Option<u64>,
    alarm_slot: AlarmSlot,
}

impl ITimer {
    pub fn new() -> Self {
        Self {
            interval_ns: 0,
            deadline_ns: None,
            alarm_slot: AlarmSlot::new(),
        }
    }

    fn remaining_ns(&self, now_ns: u64) -> u64 {
        self.deadline_ns
            .map_or(0, |deadline_ns| deadline_ns.saturating_sub(now_ns))
    }

    fn replace(&mut self, ty: ITimerType, setting: ITimerSetting, now_ns: u64) -> AlarmChange {
        self.interval_ns = setting.interval_ns;
        self.deadline_ns = (setting.remaining_ns != 0).then(|| {
            now_ns
                .saturating_add(setting.remaining_ns)
                .min(MAX_ITIMER_NANOS)
        });
        self.alarm_slot.replace(
            self.deadline_ns
                .map(|deadline_ns| itimer_alarm_delay(ty, deadline_ns.saturating_sub(now_ns))),
        )
    }

    fn update(&mut self, ty: ITimerType, now_ns: u64, triggered: bool) -> ITimerUpdate {
        let Some(deadline_ns) = self.deadline_ns else {
            return ITimerUpdate::default();
        };
        if now_ns < deadline_ns {
            ITimerUpdate {
                expired: false,
                alarm_change: triggered.then(|| {
                    self.alarm_slot
                        .replace(Some(itimer_alarm_delay(ty, deadline_ns - now_ns)))
                }),
            }
        } else {
            self.deadline_ns = (self.interval_ns != 0).then(|| {
                next_periodic_deadline(deadline_ns, self.interval_ns, now_ns).min(MAX_ITIMER_NANOS)
            });
            ITimerUpdate {
                expired: true,
                alarm_change: Some(self.alarm_slot.replace(self.deadline_ns.map(
                    |next_deadline_ns| {
                        itimer_alarm_delay(ty, next_deadline_ns.saturating_sub(now_ns))
                    },
                ))),
            }
        }
    }
}

impl Default for ITimer {
    fn default() -> Self {
        Self::new()
    }
}

include!("itimer/manager.rs");

#[derive(Default)]
struct ITimerUpdate {
    expired: bool,
    alarm_change: Option<AlarmChange>,
}

fn next_periodic_deadline(deadline_ns: u64, interval_ns: u64, now_ns: u64) -> u64 {
    let elapsed_periods = now_ns
        .saturating_sub(deadline_ns)
        .checked_div(interval_ns)
        .unwrap_or(0);
    deadline_ns.saturating_add(interval_ns.saturating_mul(elapsed_periods.saturating_add(1)))
}

fn bounded_itimer_nanos(value: TimeValue) -> u64 {
    u64::try_from(value.as_nanos().min(u128::from(MAX_ITIMER_NANOS))).unwrap_or(MAX_ITIMER_NANOS)
}

include!("itimer/tests.rs");
