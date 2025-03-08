use core::ffi::c_void;

use crate::ptr::{UserConstPtr, UserPtr};

pub(crate) fn sys_rt_sigprocmask(
    _how: i32,
    _set: UserConstPtr<c_void>,
    _oldset: UserPtr<c_void>,
    _sigsetsize: usize,
) -> isize {
    warn!("sys_rt_sigprocmask: not implemented");
    0
}

pub(crate) fn sys_rt_sigaction(
    _signum: i32,
    _act: UserConstPtr<c_void>,
    _oldact: UserPtr<c_void>,
    _sigsetsize: usize,
) -> isize {
    warn!("sys_rt_sigaction: not implemented");
    0
}
