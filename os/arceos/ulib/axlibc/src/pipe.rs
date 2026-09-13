use core::ffi::c_int;

use ax_posix_api::{PosixError, sys_pipe2};

use crate::utils::e;

/// Create a pipe
///
/// Return 0 if succeed
///
/// # Safety
/// A non-null `fd` must point to two uniquely writable, aligned integers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pipe(fd: *mut c_int) -> c_int {
    // SAFETY: pipe and pipe2 have the same output buffer contract.
    unsafe { pipe2(fd, 0) }
}

/// Creates a byte-stream pipe, optionally marking both fds close-on-exec.
///
/// # Safety
/// A non-null `fd` must point to two uniquely writable, aligned integers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pipe2(fd: *mut c_int, flags: c_int) -> c_int {
    if fd.is_null() {
        return e(-PosixError::EFAULT.errno().into_raw());
    }
    // SAFETY: the caller supplies the two writable descriptor slots.
    let fds = unsafe { core::slice::from_raw_parts_mut(fd, 2) };
    e(sys_pipe2(fds, flags))
}
