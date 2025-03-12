use core::ffi::{c_char, c_void};

use arceos_posix_api::{self as api, ctypes::mode_t};

use crate::{
    ptr::{PtrWrapper, UserConstPtr, UserPtr},
    syscall_unwrap,
};

pub(crate) fn sys_read(fd: i32, buf: UserPtr<c_void>, count: usize) -> isize {
    let buf = syscall_unwrap!(buf.get_as_bytes(count));
    api::sys_read(fd, buf, count)
}

pub(crate) fn sys_write(fd: i32, buf: UserConstPtr<c_void>, count: usize) -> isize {
    let buf = syscall_unwrap!(buf.get_as_bytes(count));
    api::sys_write(fd, buf, count)
}

pub(crate) fn sys_writev(fd: i32, iov: UserConstPtr<api::ctypes::iovec>, iocnt: i32) -> isize {
    let iov = syscall_unwrap!(iov.get_as_bytes(iocnt as _));
    unsafe { api::sys_writev(fd, iov, iocnt) }
}

pub(crate) fn sys_openat(
    dirfd: i32,
    path: UserConstPtr<c_char>,
    flags: i32,
    modes: mode_t,
) -> isize {
    let path = syscall_unwrap!(path.get_as_null_terminated());
    api::sys_openat(dirfd, path.as_ptr(), flags, modes) as _
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn sys_open(path: UserConstPtr<c_char>, flags: i32, modes: mode_t) -> isize {
    use arceos_posix_api::AT_FDCWD;
    sys_openat(AT_FDCWD as _, path, flags, modes)
}
