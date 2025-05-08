use core::ffi::{c_char, c_void};

use axerrno::{LinuxError, LinuxResult};

use crate::ptr::UserConstPtr;

pub fn sys_mount(
    _source: UserConstPtr<c_char>,
    _target: UserConstPtr<c_char>,
    _fs_type: UserConstPtr<c_char>,
    _flags: i32,
    _data: UserConstPtr<c_void>,
) -> LinuxResult<isize> {
    Err(LinuxError::ENOSYS)
}

pub fn sys_umount2(_target: UserConstPtr<c_char>, _flags: i32) -> LinuxResult<isize> {
    Err(LinuxError::ENOSYS)
}
