use core::ffi::c_int;

use arceos_posix_api as api;

use crate::{
    ptr::{PtrWrapper, UserPtr},
    syscall_body,
};

pub(crate) fn sys_pipe2(fds: UserPtr<i32>) -> c_int {
    syscall_body!(sys_pipe2, {
        let fds = fds.get_as_array(2)?;
        let fds_slice: &mut [c_int] = unsafe { core::slice::from_raw_parts_mut(fds, 2) };
        Ok(api::sys_pipe(fds_slice))
    })
}
