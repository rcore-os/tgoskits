use core::ffi::c_int;

use arceos_posix_api as api;

pub fn sys_dup(old_fd: c_int) -> c_int {
    api::sys_dup(old_fd)
}

pub fn sys_dup3(old_fd: c_int, new_fd: c_int) -> c_int {
    api::sys_dup2(old_fd, new_fd)
}

pub fn sys_close(fd: c_int) -> c_int {
    api::sys_close(fd)
}

pub fn sys_fcntl(fd: c_int, cmd: c_int, arg: usize) -> c_int {
    api::sys_fcntl(fd, cmd, arg)
}
