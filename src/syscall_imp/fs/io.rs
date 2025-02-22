use core::ffi::{c_char, c_void};

use arceos_posix_api::{self as api, ctypes::mode_t};

pub(crate) fn sys_read(fd: i32, buf: *mut c_void, count: usize) -> isize {
    api::sys_read(fd, buf, count)
}

pub(crate) fn sys_write(fd: i32, buf: *const c_void, count: usize) -> isize {
    api::sys_write(fd, buf, count)
}

pub(crate) fn sys_writev(fd: i32, iov: *const api::ctypes::iovec, iocnt: i32) -> isize {
    unsafe { api::sys_writev(fd, iov, iocnt) }
}

pub(crate) fn sys_openat(dirfd: i32, path: *const c_char, flags: i32, modes: mode_t) -> isize {
    api::sys_openat(dirfd, path, flags, modes) as isize
}
