use super::syscall::*;

#[repr(C)]
#[derive(Default)]
pub struct TimeSpec {
    /// seconds
    pub sec: usize,
    /// nano seconds
    pub nsec: usize,
}

pub type ClockId = u32;

pub const CLOCK_REALTIME: ClockId = 0;
pub const CLOCK_MONOTONIC: ClockId = 1;

pub const TIMER_ABSTIME: u32 = 1;

pub fn clock_gettime(clk: ClockId, req: &mut TimeSpec) -> isize {
    sys_clock_gettime(clk, req)
}

pub fn get_time_us() -> isize {
    let mut req = TimeSpec::default();
    let ret = clock_gettime(CLOCK_REALTIME, &mut req);
    if ret < 0 {
        ret
    } else {
        (req.sec * 1_000_000 + req.nsec / 1_000) as isize
    }
}

pub fn clock_nanosleep(clk: ClockId, flags: u32, req: &TimeSpec) -> isize {
    sys_clock_nanosleep(clk, flags, req)
}

pub fn nanosleep(req: &TimeSpec) -> isize {
    clock_nanosleep(CLOCK_REALTIME, 0, req)
}

pub fn usleep(useconds: usize) -> isize {
    nanosleep(&TimeSpec {
        sec: useconds / 1_000_000,
        nsec: (useconds % 1_000_000) * 1_000,
    })
}

pub fn sleep(seconds: usize) -> isize {
    let tv = TimeSpec {
        sec: seconds,
        nsec: 0,
    };
    if nanosleep(&tv) != 0 {
        tv.sec as _
    } else {
        0
    }
}
