use ax_std::os::arceos::{api, modules};

pub fn wall_time() -> api::time::AxTimeValue {
    api::time::ax_wall_time()
}

pub fn current_ticks() -> u64 {
    modules::ax_hal::time::current_ticks()
}

pub fn ticks_to_nanos(ticks: u64) -> u64 {
    modules::ax_hal::time::ticks_to_nanos(ticks)
}

pub fn nanos_to_ticks(nanos: u64) -> u64 {
    modules::ax_hal::time::nanos_to_ticks(nanos)
}

pub fn monotonic_time() -> core::time::Duration {
    modules::ax_hal::time::monotonic_time()
}

#[cfg(target_arch = "x86_64")]
pub fn monotonic_time_nanos() -> u64 {
    modules::ax_hal::time::monotonic_time_nanos()
}

pub fn set_oneshot_timer(deadline_ns: u64) {
    modules::ax_hal::time::set_oneshot_timer(deadline_ns);
}

pub fn busy_wait(dur: core::time::Duration) {
    modules::ax_hal::time::busy_wait(dur);
}
