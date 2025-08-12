use core::ffi::c_char;

use axerrno::{LinuxError, LinuxResult};
use axtask::current;
use linux_raw_sys::{
    general::{__user_cap_data_struct, __user_cap_header_struct},
    prctl::{PR_GET_NAME, PR_SET_NAME},
};
use starry_core::task::{AsThread, get_process_data};

use crate::mm::{UserConstPtr, UserPtr};

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

pub fn sys_get_mempolicy(
    _policy: UserPtr<i32>,
    _nodemask: UserPtr<usize>,
    _maxnode: usize,
    _addr: usize,
    _flags: usize,
) -> LinuxResult<isize> {
    warn!("Dummy get_mempolicy called");
    Ok(0)
}

pub fn sys_prctl(
    option: u32,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> LinuxResult<isize> {
    debug!(
        "sys_prctl <= option: {}, args: {}, {}, {}, {}",
        option, arg2, arg3, arg4, arg5
    );

    match option {
        PR_SET_NAME => {
            let s = UserConstPtr::<c_char>::from(arg2).get_as_str()?;
            current().set_name(s);
        }
        PR_GET_NAME => {
            let name = current().name();
            let dst = UserPtr::<u8>::from(arg2).get_as_mut_slice(16)?;
            let copy_len = name.len().min(15);
            dst[..copy_len].copy_from_slice(&name.as_bytes()[..copy_len]);
            dst[copy_len] = 0;
        }
        _ => {
            warn!("sys_prctl: unsupported option {}", option);
            return Err(LinuxError::EINVAL);
        }
    }

    Ok(0)
}
