use axerrno::{AxError, AxResult};
use starry_core::task::{get_process_data, send_signal_to_process};
use starry_signal::SignalInfo;

use crate::{
    file::{FD_TABLE, FileLike, PidFd, add_file_like},
    syscall::signal::make_queue_signal_info,
};

pub fn sys_pidfd_open(pid: u32, flags: u32) -> AxResult<isize> {
    debug!("sys_pidfd_open <= pid: {pid}, flags: {flags}");

    if flags != 0 {
        return Err(AxError::InvalidInput);
    }

    let task = get_process_data(pid)?;
    let fd = PidFd::new(&task);

    fd.add_to_fd_table(true).map(|fd| fd as _)
}

pub fn sys_pidfd_getfd(pidfd: i32, target_fd: i32, flags: u32) -> AxResult<isize> {
    debug!("sys_pidfd_getfd <= pidfd: {pidfd}, target_fd: {target_fd}, flags: {flags}");

    let pidfd = PidFd::from_fd(pidfd)?;
    let proc_data = pidfd.process_data()?;
    FD_TABLE
        .scope(&proc_data.scope.read())
        .read()
        .get(target_fd as usize)
        .ok_or(AxError::BadFileDescriptor)
        .and_then(|fd| {
            let fd = add_file_like(fd.inner.clone(), true)?;
            Ok(fd as isize)
        })
}

pub fn sys_pidfd_send_signal(
    pidfd: i32,
    signo: u32,
    sig: *mut SignalInfo,
    flags: u32,
) -> AxResult<isize> {
    if flags != 0 {
        return Err(AxError::InvalidInput);
    }

    let pidfd = PidFd::from_fd(pidfd)?;
    let pid = pidfd.process_data()?.proc.pid();

    let sig = make_queue_signal_info(pid, signo, sig)?;
    send_signal_to_process(pid, sig)?;
    Ok(0)
}
