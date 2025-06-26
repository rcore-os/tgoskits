use axerrno::LinuxResult;
use linux_raw_sys::net::socklen_t;

use crate::ptr::UserPtr;

pub fn sys_getsockopt(
    fd: i32,
    level: u32,
    optname: u32,
    optval: UserPtr<u8>,
    optlen: socklen_t,
) -> LinuxResult<isize> {
    debug!(
        "sys_getsockopt <= fd: {}, level: {}, optname: {}, optval: {:?}, optlen: {}",
        fd,
        level,
        optname,
        optval.address(),
        optlen
    );

    *optval.cast::<u32>().get_as_mut()? = 0;
    Ok(0)
}

pub fn sys_setsockopt(
    fd: i32,
    level: i32,
    optname: i32,
    optval: UserPtr<u8>,
    optlen: socklen_t,
) -> LinuxResult<isize> {
    debug!(
        "sys_setsockopt <= fd: {}, level: {}, optname: {}, optval: {:?}, optlen: {}",
        fd,
        level,
        optname,
        optval.address(),
        optlen
    );

    Ok(0)
}
