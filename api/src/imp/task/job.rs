use axerrno::{LinuxError, LinuxResult};
use axprocess::Pid;
use axtask::current;
use starry_core::task::{StarryTaskExt, get_process, get_process_group};

pub fn sys_setsid() -> LinuxResult<isize> {
    let curr = current();
    let process = StarryTaskExt::of(&curr).thread.process();
    if get_process_group(process.pid()).is_ok() {
        return Err(LinuxError::EPERM);
    }

    if let Some((session, _)) = process.create_session() {
        Ok(session.sid() as _)
    } else {
        Ok(process.pid() as _)
    }
}

pub fn sys_getpgid(pid: Pid) -> LinuxResult<isize> {
    Ok(if pid == 0 {
        StarryTaskExt::of(&current()).thread.process().group()
    } else {
        get_process(pid)?.group()
    }
    .pgid() as _)
}

pub fn sys_setpgid(pid: Pid, pgid: Pid) -> LinuxResult<isize> {
    let curr = current();

    let process = if pid == 0 {
        StarryTaskExt::of(&curr).thread.process()
    } else {
        &get_process(pid)?
    };

    if pgid == 0 {
        process.create_group();
    } else if !process.move_to_group(&get_process_group(pgid)?) {
        return Err(LinuxError::EPERM);
    }

    Ok(0)
}

// TODO: job control
