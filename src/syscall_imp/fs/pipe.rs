use core::ffi::c_int;

use arceos_posix_api as api;

pub(crate) fn sys_pipe2(fds: *mut i32) -> c_int {
    let fds_slice: &mut [c_int] = unsafe { core::slice::from_raw_parts_mut(fds, 2) };
    api::sys_pipe(fds_slice)
}
