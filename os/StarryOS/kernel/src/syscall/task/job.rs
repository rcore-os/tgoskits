use ax_errno::{AxError, AxResult};
use ax_task::current;
use starry_process::Pid;

use crate::task::{
    AsThread, get_process_data, get_process_group, get_zombie_process, register_process_group,
    register_session,
};

pub fn sys_getsid(pid: Pid) -> AxResult<isize> {
    if let Ok(data) = get_process_data(pid) {
        return Ok(data.proc.group().session().sid() as _);
    }
    // Zombie: ProcessData is gone but Arc<Process> is still held in ZOMBIE_PIDS.
    if let Some(proc) = get_zombie_process(pid) {
        return Ok(proc.group().session().sid() as _);
    }
    Err(AxError::NoSuchProcess)
}

pub fn sys_setsid() -> AxResult<isize> {
    let curr = current();
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
    if let Ok(data) = get_process_data(pid) {
        return Ok(data.proc.group().pgid() as _);
    }
    // Zombie: ProcessData is gone but Arc<Process> is still held in ZOMBIE_PIDS.
    if let Some(proc) = get_zombie_process(pid) {
        return Ok(proc.group().pgid() as _);
    }
    Err(AxError::NoSuchProcess)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_getpgrp() -> AxResult<isize> {
    let curr = current();
    Ok(curr.as_thread().proc_data.proc.group().pgid() as _)
}

pub fn sys_setpgid(pid: Pid, pgid: Pid) -> AxResult<isize> {
    let proc = &get_process_data(pid)?.proc;

    if pgid == 0 || pgid == proc.pid() {
        if let Some(pg) = proc.create_group() {
            register_process_group(&pg);
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
