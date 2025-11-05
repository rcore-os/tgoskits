use core::ffi::c_char;

use axerrno::{AxError, AxResult};
use axtask::current;
use linux_raw_sys::general::{__user_cap_data_struct, __user_cap_header_struct};
use starry_core::task::{AsThread, get_process_data};
use starry_vm::{VmMutPtr, VmPtr, vm_write_slice};

use crate::mm::vm_load_string;

const CAPABILITY_VERSION_3: u32 = 0x20080522;

fn validate_cap_header(header_ptr: *mut __user_cap_header_struct) -> AxResult<()> {
    // FIXME: AnyBitPattern
    let mut header = unsafe { header_ptr.vm_read_uninit()?.assume_init() };
    if header.version != CAPABILITY_VERSION_3 {
        header.version = CAPABILITY_VERSION_3;
        header_ptr.vm_write(header)?;
        return Err(AxError::InvalidInput);
    }
    let _ = get_process_data(header.pid as u32)?;
    Ok(())
}

pub fn sys_capget(
    header: *mut __user_cap_header_struct,
    data: *mut __user_cap_data_struct,
) -> AxResult<isize> {
    validate_cap_header(header)?;

    data.vm_write(__user_cap_data_struct {
        effective: u32::MAX,
        permitted: u32::MAX,
        inheritable: u32::MAX,
    })?;
    Ok(0)
}

pub fn sys_capset(
    header: *mut __user_cap_header_struct,
    _data: *mut __user_cap_data_struct,
) -> AxResult<isize> {
    validate_cap_header(header)?;

    Ok(0)
}

pub fn sys_umask(mask: u32) -> AxResult<isize> {
    let curr = current();
    let old = curr.as_thread().proc_data.replace_umask(mask);
    Ok(old as isize)
}

pub fn sys_setreuid(_ruid: u32, _euid: u32) -> AxResult<isize> {
    Ok(0)
}

pub fn sys_setresuid(_ruid: u32, _euid: u32, _suid: u32) -> AxResult<isize> {
    Ok(0)
}

pub fn sys_setresgid(_rgid: u32, _egid: u32, _sgid: u32) -> AxResult<isize> {
    Ok(0)
}

pub fn sys_get_mempolicy(
    _policy: *mut i32,
    _nodemask: *mut usize,
    _maxnode: usize,
    _addr: usize,
    _flags: usize,
) -> AxResult<isize> {
    warn!("Dummy get_mempolicy called");
    Ok(0)
}

pub fn sys_prctl(
    option: u32,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> AxResult<isize> {
    use linux_raw_sys::prctl::*;

    debug!("sys_prctl <= option: {option}, args: {arg2}, {arg3}, {arg4}, {arg5}");

    match option {
        PR_SET_NAME => {
            let s = vm_load_string(arg2 as *const c_char)?;
            current().set_name(&s);
        }
        PR_GET_NAME => {
            let name = current().name();
            let len = name.len().min(15);
            let mut buf = [0; 16];
            buf[..len].copy_from_slice(&name.as_bytes()[..len]);
            vm_write_slice(arg2 as _, &buf)?;
        }
        PR_SET_SECCOMP => {}
        PR_MCE_KILL => {}
        PR_SET_MM_START_CODE
        | PR_SET_MM_END_CODE
        | PR_SET_MM_START_DATA
        | PR_SET_MM_END_DATA
        | PR_SET_MM_START_BRK
        | PR_SET_MM_START_STACK => {}
        _ => {
            warn!("sys_prctl: unsupported option {option}");
            return Err(AxError::InvalidInput);
        }
    }

    Ok(0)
}
