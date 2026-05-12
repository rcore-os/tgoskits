use core::time::Duration;

use ax_api::modules::ax_hal::time::wall_time_nanos;
use ax_posix_api::ctypes::{clockid_t, timespec};
use log::info;
use rand::{RngCore, SeedableRng, prelude::SmallRng};

/// Fill `len` bytes in `buf` with cryptographically secure random data.
///
/// Returns either the number of bytes written to buf (a positive value) or
/// * `-EINVAL` if `flags` contains unknown flags.
/// * `-ENOSYS` if the system does not support random data generation.
#[unsafe(no_mangle)]
pub fn sys_read_entropy(buf: *mut u8, len: usize, _flags: u32) -> isize {
    // flags are currently ignored
    info!("called sys_read_entropy");
    let buffer = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    let mut rng = SmallRng::seed_from_u64(wall_time_nanos());
    rng.fill_bytes(buffer);
    len as isize
}

#[unsafe(no_mangle)]
pub fn sys_clock_gettime(clockid: clockid_t, tp: *mut timespec) -> i32 {
    info!(
        "called sys_clock_gettime with clockid {}, tp {:p}",
        clockid, tp
    );
    unsafe { ax_posix_api::sys_clock_gettime(clockid, tp) }
}

/// suspend execution for microsecond intervals
///
/// The usleep() function suspends execution of the calling
/// thread for (at least) `usec` microseconds.
#[unsafe(no_mangle)]
pub fn sys_usleep(usec: u64) {
    info!("called sys_usleep with {} usec", usec);
    let duration = Duration::from_micros(usec);
    #[cfg(feature = "multitask")]
    ax_api::modules::ax_task::sleep(duration);
    #[cfg(not(feature = "multitask"))]
    ax_api::modules::ax_hal::time::busy_wait(duration);
}

/// dummy implementation of futex wait
#[cfg(not(feature = "multitask"))]
#[unsafe(no_mangle)]
pub fn sys_futex_wait(
    _address: *mut u32,
    _expected: u32,
    _timeout: *const timespec,
    _flags: u32,
) -> i32 {
    0
}

/// dummy implementation of futex wake
#[cfg(not(feature = "multitask"))]
#[unsafe(no_mangle)]
pub fn sys_futex_wake(_address: *mut u32, _count: i32) -> i32 {
    0
}
