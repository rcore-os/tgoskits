use axerrno::{LinuxError, LinuxResult};
use axtask::current;
use linux_raw_sys::general::{__user_cap_data_struct, __user_cap_header_struct};
use starry_core::task::{AsThread, get_process_data};

use crate::mm::UserPtr;

fn validate_cap_header(header: &mut __user_cap_header_struct) -> LinuxResult<()> {
    if header.version != 0x20080522 {
        header.version = 0x20080522;
        return Err(LinuxError::EINVAL);
    }
    let _ = get_process_data(header.pid as u32)?;
    Ok(())
}

pub fn sys_capget(
    header: UserPtr<__user_cap_header_struct>,
    data: UserPtr<__user_cap_data_struct>,
) -> LinuxResult<isize> {
    let header = header.get_as_mut()?;
    validate_cap_header(header)?;

    *data.get_as_mut()? = __user_cap_data_struct {
        effective: u32::MAX,
        permitted: u32::MAX,
        inheritable: u32::MAX,
    };
    Ok(0)
}

pub fn sys_capset(
    header: UserPtr<__user_cap_header_struct>,
    _data: UserPtr<__user_cap_data_struct>,
) -> LinuxResult<isize> {
    let header = header.get_as_mut()?;
    validate_cap_header(header)?;

    Ok(0)
}

pub fn sys_umask(mask: u32) -> LinuxResult<isize> {
    let curr = current();
    let old = curr.as_thread().proc_data.replace_umask(mask);
    Ok(old as isize)
}

pub fn sys_setreuid(_ruid: u32, _euid: u32) -> LinuxResult<isize> {
    Ok(0)
}

pub fn sys_setresuid(_ruid: u32, _euid: u32, _suid: u32) -> LinuxResult<isize> {
    Ok(0)
}

pub fn sys_setresgid(_rgid: u32, _egid: u32, _sgid: u32) -> LinuxResult<isize> {
    Ok(0)
}
