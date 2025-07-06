use core::mem;

use axhal::time::{NANOS_PER_SEC, TimeValue};
use axsignal::Signo;
use strum::FromRepr;

fn time_value_from_nanos(nanos: usize) -> TimeValue {
    let secs = nanos as u64 / NANOS_PER_SEC;
    let nsecs = nanos as u64 - secs * NANOS_PER_SEC;
    TimeValue::new(secs, nsecs as u32)
}

#[repr(i32)]
#[allow(non_camel_case_types)]
#[derive(Eq, PartialEq, Debug, Clone, Copy, FromRepr)]
/// The type of interval timer.
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

#[derive(Default)]
struct ITimer {
    interval_ns: usize,
    remained_ns: usize,
}

impl ITimer {
    pub fn update(&mut self, delta: usize) -> bool {
        if self.remained_ns == 0 {
            return false;
        }
        if self.remained_ns > delta {
            self.remained_ns -= delta;
            false
        } else {
            self.remained_ns = self.interval_ns;
            true
        }
    }
}

pub struct TimeManager {
    utime_ns: usize,
    stime_ns: usize,
    user_timestamp: usize,
    kernel_timestamp: usize,
    itimers: [ITimer; 3],
}

impl Default for TimeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeManager {
    pub fn new() -> Self {
        Self {
            utime_ns: 0,
            stime_ns: 0,
            user_timestamp: 0,
            kernel_timestamp: 0,
            itimers: Default::default(),
        }
    }

    pub fn output(&self) -> (TimeValue, TimeValue) {
        let utime = time_value_from_nanos(self.utime_ns);
        let stime = time_value_from_nanos(self.stime_ns);
        (utime, stime)
    }

    pub fn reset(&mut self, current_timestamp: usize) {
        self.utime_ns = 0;
        self.stime_ns = 0;
        self.user_timestamp = 0;
        self.kernel_timestamp = current_timestamp;
    }

    pub fn switch_into_kernel_mode(&mut self, current_timestamp: usize, emitter: impl Fn(Signo)) {
        let now_time_ns = current_timestamp;
        let delta = now_time_ns - self.kernel_timestamp;
        self.utime_ns += delta;
        self.kernel_timestamp = now_time_ns;
        self.update_itimer(ITimerType::Real, delta, &emitter);
        self.update_itimer(ITimerType::Virtual, delta, &emitter);
        self.update_itimer(ITimerType::Prof, delta, &emitter);
    }

    pub fn switch_into_user_mode(&mut self, current_timestamp: usize, emitter: impl Fn(Signo)) {
        let now_time_ns = current_timestamp;
        let delta = now_time_ns - self.kernel_timestamp;
        self.stime_ns += delta;
        self.user_timestamp = now_time_ns;
        self.update_itimer(ITimerType::Real, delta, &emitter);
        self.update_itimer(ITimerType::Prof, delta, &emitter);
    }

    // TODO: why nobody calls this?
    pub fn switch_from_old_task(&mut self, current_timestamp: usize, emitter: impl Fn(Signo)) {
        let now_time_ns = current_timestamp;
        let delta = now_time_ns - self.kernel_timestamp;
        self.stime_ns += delta;
        self.kernel_timestamp = now_time_ns;
        self.update_itimer(ITimerType::Real, delta, &emitter);
        self.update_itimer(ITimerType::Prof, delta, &emitter);
    }

    // TODO: why nobody calls this?
    pub fn switch_to_new_task(&mut self, current_timestamp: usize, emitter: impl Fn(Signo)) {
        let now_time_ns = current_timestamp;
        let delta = now_time_ns - self.kernel_timestamp;
        self.kernel_timestamp = now_time_ns;
        self.update_itimer(ITimerType::Real, delta, &emitter);
    }

    pub fn set_itimer(
        &mut self,
        ty: ITimerType,
        interval_ns: usize,
        remained_ns: usize,
    ) -> (TimeValue, TimeValue) {
        let old = mem::replace(
            &mut self.itimers[ty as usize],
            ITimer {
                interval_ns,
                remained_ns,
            },
        );
        (
            time_value_from_nanos(old.interval_ns),
            time_value_from_nanos(old.remained_ns),
        )
    }

    pub fn get_itimer(&self, ty: ITimerType) -> (TimeValue, TimeValue) {
        let itimer = &self.itimers[ty as usize];
        (
            time_value_from_nanos(itimer.interval_ns),
            time_value_from_nanos(itimer.remained_ns),
        )
    }

    fn update_itimer(&mut self, ty: ITimerType, delta: usize, emitter: impl Fn(Signo)) {
        if self.itimers[ty as usize].update(delta) {
            emitter(ty.signo());
        }
    }
}
