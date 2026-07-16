use ax_errno::{AxError, AxResult};
use starry_process::Pid;

use crate::task::{
    current_user_task, get_process, get_process_data, get_process_group, register_process_group,
    register_session,
};

pub fn sys_getsid(pid: Pid) -> AxResult<isize> {
    Ok(get_process(pid)?.group().session().sid() as _)
}

pub fn sys_setsid() -> AxResult<isize> {
    let curr = current_user_task();
    let proc = &curr.as_thread().proc_data.proc;
    if get_process_group(proc.pid()).is_ok() {
        return Err(AxError::OperationNotPermitted);
    }

    if let Some((session, pg)) = proc.create_session() {
        register_session(&session);
        register_process_group(&pg);
        Ok(session.sid() as _)
    } else {
        Ok(proc.pid() as _)
    }
}

pub fn sys_getpgid(pid: Pid) -> AxResult<isize> {
    Ok(get_process(pid)?.group().pgid() as _)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_getpgrp() -> AxResult<isize> {
    let curr = current_user_task();
    Ok(curr.as_thread().proc_data.proc.group().pgid() as _)
}

pub fn sys_setpgid(pid: Pid, pgid: Pid) -> AxResult<isize> {
    let proc = &get_process_data(pid)?.proc;

    if pgid == 0 || pgid == proc.pid() {
        if let Some(pg) = proc.create_group() {
            register_process_group(&pg);
        } else {
            register_process_group(&proc.group());
        }
    } else {
        // POSIX: looking up a non-existent target pgid yields EPERM,
        // not ESRCH (which is reserved for pid lookup failures).
        let group = get_process_group(pgid).map_err(|_| AxError::OperationNotPermitted)?;
        if !proc.move_to_group(&group) {
            return Err(AxError::OperationNotPermitted);
        }
    }

    Ok(0)
}

// TODO: job control
