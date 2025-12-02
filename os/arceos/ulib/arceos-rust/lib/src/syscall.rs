use ax_api::modules::ax_hal::time::wall_time_nanos;
use ax_api::modules::ax_log::{ax_println, info};
use ax_api::sys::ax_terminate;
use ax_posix_api::ctypes::timespec;
use rand::rngs::SmallRng;
use rand::{RngCore, SeedableRng};

#[unsafe(no_mangle)]
pub fn sys_futex_wait(
    address: *mut u32,
    expected: u32,
    timeout: *const timespec,
    flags: u32,
) -> i32 {
    // sys_futex_wait(address, expected, timeout, flags);
    // Placeholder implementation
    // info!("called sys_futex_wait");
    0
}

#[unsafe(no_mangle)]
pub fn sys_futex_wake(address: *mut u32, count: i32) -> i32 {
    // Placeholder implementation
    info!("called sys_futex_wake");
    0
}

#[unsafe(no_mangle)]
pub fn sys_read_entropy(buf: *mut u8, len: usize, flags: u32) -> isize {
    // TODO: flags are currently ignored
    info!("called sys_read_entropy");
    let buffer = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    let mut rng = SmallRng::seed_from_u64(wall_time_nanos());
    rng.fill_bytes(buffer);
    len as isize
}
