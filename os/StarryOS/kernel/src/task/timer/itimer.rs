use super::*;

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
}

fn itimer_alarm_delay(ty: ITimerType, remained_ns: usize) -> Duration {
    let divisor = match ty {
        ITimerType::Real => 1,
        // Process CPU time may advance concurrently on every configured CPU.
        // Waking conservatively early keeps the task-context worker from
        // delivering a CPU timer late without putting POSIX timer callbacks in
        // the hard-IRQ path.
        ITimerType::Virtual | ITimerType::Prof => ax_runtime::CPU_CAPACITY.max(1),
    };
    Duration::from_nanos(remained_ns.div_ceil(divisor).max(1) as u64)
}

struct ITimer {
    interval_ns: usize,
    remained_ns: usize,
    alarm_slot: AlarmSlot,
}

impl ITimer {
    pub fn new(interval_ns: usize, remained_ns: usize) -> Self {
        Self {
            interval_ns,
            remained_ns,
            alarm_slot: AlarmSlot::new(),
        }
    }

    pub fn update(&mut self, ty: ITimerType, delta: usize, triggered: bool) -> ITimerUpdate {
        if self.remained_ns == 0 {
            return ITimerUpdate::default();
        }
        if self.remained_ns > delta {
            self.remained_ns -= delta;
            ITimerUpdate {
                expired: false,
                alarm_change: triggered.then(|| {
                    self.alarm_slot
                        .replace(Some(itimer_alarm_delay(ty, self.remained_ns)))
                }),
            }
        } else {
            self.remained_ns = self.interval_ns;
            ITimerUpdate {
                expired: true,
                alarm_change: Some(self.alarm_slot.replace(
                    (self.remained_ns > 0).then(|| itimer_alarm_delay(ty, self.remained_ns)),
                )),
            }
        }
    }
}

impl Default for ITimer {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

include!("itimer/manager.rs");

#[derive(Default)]
struct ITimerUpdate {
    expired: bool,
    alarm_change: Option<AlarmChange>,
}

include!("itimer/tests.rs");
