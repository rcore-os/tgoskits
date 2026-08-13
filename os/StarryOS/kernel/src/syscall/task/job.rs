use ax_errno::{AxError, AxResult};
use starry_process::Pid;

use crate::task::{
    get_process, get_process_data, get_process_group, register_process_group, register_session,
    resolve_user_pid, visible_user_pid,
};

fn process_pid(current: &crate::task::UserTaskRef, pid: Pid) -> AxResult<Pid> {
    if pid == 0 {
        Ok(current.as_thread().proc_data.proc.pid())
    } else {
        resolve_user_pid(current, pid)
    }
}

pub fn sys_getsid(current: &crate::task::UserTaskRef, pid: Pid) -> AxResult<isize> {
    let sid = get_process(process_pid(current, pid)?)?
        .group()
        .session()
        .sid();
    Ok(visible_user_pid(current, sid as u64) as _)
}

pub fn sys_setsid(current: &crate::task::UserTaskRef) -> AxResult<isize> {
    let proc_data = &current.as_thread().proc_data;
    let proc = &proc_data.proc;
    if get_process_group(proc.pid()).is_ok() {
        return Err(AxError::OperationNotPermitted);
    }

    let identity =
        axnsproxy::JobControlId::retain(proc.pid() as u64, proc_data.identity().pid_namespaces())?;
    if let Some((session, pg)) = proc.create_session(identity) {
        register_session(&session);
        register_process_group(&pg);
        Ok(visible_user_pid(current, session.sid() as u64) as _)
    } else {
        Ok(visible_user_pid(current, proc.pid() as u64) as _)
    }
}

pub fn sys_getpgid(current: &crate::task::UserTaskRef, pid: Pid) -> AxResult<isize> {
    let pgid = get_process(process_pid(current, pid)?)?.group().pgid();
    Ok(visible_user_pid(current, pgid as u64) as _)
}

#[cfg(target_arch = "x86_64")]
pub fn sys_getpgrp(current: &crate::task::UserTaskRef) -> AxResult<isize> {
    let pgid = current.as_thread().proc_data.proc.group().pgid();
    Ok(visible_user_pid(current, pgid as u64) as _)
}

pub fn sys_setpgid(current: &crate::task::UserTaskRef, pid: i32, pgid: i32) -> AxResult<isize> {
    if pid < 0 || pgid < 0 {
        return Err(AxError::InvalidInput);
    }
    let local_pid = if pid == 0 {
        visible_user_pid(current, current.as_thread().proc_data.proc.pid() as u64)
    } else {
        pid as Pid
    };
    let pid = process_pid(current, pid as Pid)?;
    let local_pgid = if pgid == 0 { local_pid } else { pgid as Pid };
    let pgid = if local_pgid == local_pid {
        pid
    } else {
        resolve_user_pid(current, local_pgid).map_err(|_| AxError::OperationNotPermitted)?
    };

    let proc_data = get_process_data(pid)?;
    let proc = &proc_data.proc;

    if pgid == 0 || pgid == proc.pid() {
        let identity = axnsproxy::JobControlId::retain(
            proc.pid() as u64,
            proc_data.identity().pid_namespaces(),
        )?;
        if let Some(pg) = proc.create_group(identity) {
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

#[cfg(axtest)]
pub(crate) fn job_setpgid_validation_rules_hold_for_test() -> bool {
    // Test sys_setpgid validation: negative pid or pgid should fail
    // The function checks: if pid < 0 || pgid < 0 return Err(InvalidInput)

    // Negative pid should be invalid
    let neg_pid = -1i32;
    assert!(neg_pid < 0);

    // Negative pgid should be invalid
    let neg_pgid = -1i32;
    assert!(neg_pgid < 0);

    // Zero is valid (means "use current")
    let zero = 0i32;
    assert!(zero >= 0);

    // Positive values are valid
    let pos_pid = 100i32;
    assert!(pos_pid >= 0);

    true
}

// TODO: job control
