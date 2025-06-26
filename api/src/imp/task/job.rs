use axerrno::{LinuxError, LinuxResult};
use axtask::current;
use starry_core::task::{StarryTaskExt, get_process_group};

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

// TODO: job control
