use core::ffi::c_int;

use crate::{backend::sys_pipe, utils::e};

/// Create a pipe
///
/// Return 0 if succeed
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pipe(fd: *mut c_int) -> c_int {
    let fds = unsafe { core::slice::from_raw_parts_mut(fd, 2) };
    e(sys_pipe(fds))
}
