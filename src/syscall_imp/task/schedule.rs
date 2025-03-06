use arceos_posix_api as api;

use crate::{
    ptr::{PtrWrapper, UserConstPtr, UserPtr},
    syscall_body,
};

pub(crate) fn sys_sched_yield() -> i32 {
    api::sys_sched_yield()
}

pub(crate) fn sys_nanosleep(
    req: UserConstPtr<api::ctypes::timespec>,
    rem: UserPtr<api::ctypes::timespec>,
) -> i32 {
    syscall_body!(sys_nanosleep, unsafe {
        Ok(api::sys_nanosleep(req.get()?, rem.get()?))
    })
}
