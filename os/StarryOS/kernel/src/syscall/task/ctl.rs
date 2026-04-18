use core::ffi::c_char;

use ax_errno::{AxError, AxResult};
use ax_task::current;
use linux_raw_sys::general::{__user_cap_data_struct, __user_cap_header_struct};
use starry_vm::{VmMutPtr, VmPtr, vm_write_slice};

use crate::{
    mm::vm_load_string,
    task::{AsThread, Cred, get_process_data, get_task},
};

const CAPABILITY_VERSION_3: u32 = 0x20080522;

/// Validate the cap header and return the target pid (0 means self).
fn validate_cap_header(header_ptr: *mut __user_cap_header_struct) -> AxResult<u32> {
    // FIXME: AnyBitPattern
    let mut header = unsafe { header_ptr.vm_read_uninit()?.assume_init() };
    if header.version != CAPABILITY_VERSION_3 {
        header.version = CAPABILITY_VERSION_3;
        header_ptr.vm_write(header)?;
        return Err(AxError::InvalidInput);
    }
    let pid = header.pid as u32;
    let _ = get_process_data(pid)?;
    Ok(pid)
}

/// Read the credential set for the thread identified by TID (0 = self).
///
/// capget(2) operates on the thread identified by `header.pid`; on Linux
/// threads in the same thread group share the same `struct cred` by default,
/// so reading any thread's cred gives the same answer.
fn cred_for_pid(pid: u32) -> AxResult<alloc::sync::Arc<Cred>> {
    if pid == 0 {
        return Ok(current().as_thread().cred());
    }
    let task = get_task(pid).map_err(|_| AxError::NoSuchProcess)?;
    task.try_as_thread()
        .map(|t| t.cred())
        .ok_or(AxError::NoSuchProcess)
}

pub fn sys_capget(
    header: *mut __user_cap_header_struct,
    data: *mut __user_cap_data_struct,
) -> AxResult<isize> {
    let pid = validate_cap_header(header)?;

    let cred = cred_for_pid(pid)?;
    let caps = if cred.euid == 0 { u32::MAX } else { 0 };
    let data_struct = __user_cap_data_struct {
        effective: caps,
        permitted: caps,
        inheritable: caps,
    };
    // Capability version 3 uses an array of TWO __user_cap_data_struct
    // entries (low 32 bits and high 32 bits). Write both.
    unsafe {
        (data as *mut __user_cap_data_struct).vm_write(data_struct)?;
        (data as *mut __user_cap_data_struct)
            .add(1)
            .vm_write(data_struct)?;
    }
    Ok(0)
}

pub fn sys_capset(
    header: *mut __user_cap_header_struct,
    _data: *mut __user_cap_data_struct,
) -> AxResult<isize> {
    let _ = validate_cap_header(header)?;

    let cred = current().as_thread().cred();
    if cred.euid != 0 {
        return Err(AxError::OperationNotPermitted);
    }
    // For now, accept and ignore the values (no real capability tracking).
    Ok(0)
}

pub fn sys_umask(mask: u32) -> AxResult<isize> {
    let curr = current();
    let old = curr.as_thread().proc_data.replace_umask(mask);
    Ok(old as isize)
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

/// prctl() is called with a first argument describing what to do, and further
/// arguments with a significance depending on the first one.
/// The first argument can be:
/// - PR_SET_NAME: set the name of the calling thread, using the value pointed to by `arg2`
/// - PR_GET_NAME: get the name of the calling
/// - PR_SET_SECCOMP: enable seccomp mode, with the mode specified in `arg2`
/// - PR_MCE_KILL: set the machine check exception policy
/// - PR_SET_MM options: set various memory management options (start/end code/data/brk/stack)
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
        PR_SET_MM => {
            // not implemented; but avoid annoying warnings
            return Err(AxError::InvalidInput);
        }
        _ => {
            warn!("sys_prctl: unsupported option {option}");
            return Err(AxError::InvalidInput);
        }
    }

    Ok(0)
}
