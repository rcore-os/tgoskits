use core::ffi::c_int;

use arceos_posix_api as api;

pub(crate) fn sys_dup(old_fd: c_int) -> c_int {
    api::sys_dup(old_fd)
}

pub(crate) fn sys_dup3(old_fd: c_int, new_fd: c_int) -> c_int {
    api::sys_dup2(old_fd, new_fd)
}

pub(crate) fn sys_close(fd: c_int) -> c_int {
    api::sys_close(fd)
}
