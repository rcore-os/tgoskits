use axerrno::{LinuxError, LinuxResult};
use axprocess::Pid;
use axtask::{TaskExtRef, current};
use linux_raw_sys::general::{RLIM_NLIMITS, rlimit64};
use starry_core::task::{ProcessData, get_process};

use crate::ptr::{UserConstPtr, UserPtr, nullable};

pub fn sys_prlimit64(
    pid: Pid,
    resource: u32,
    new_limit: UserConstPtr<rlimit64>,
    old_limit: UserPtr<rlimit64>,
) -> LinuxResult<isize> {
    if resource >= RLIM_NLIMITS {
        return Err(LinuxError::EINVAL);
    }

    let proc = if pid == 0 {
        current().task_ext().thread.process().clone()
    } else {
        get_process(pid)?
    };
    let proc_data: &ProcessData = proc.data().unwrap();
    if let Some(old_limit) = nullable!(old_limit.get_as_mut())? {
        let limit = &proc_data.rlim.read()[resource];
        old_limit.rlim_cur = limit.current;
        old_limit.rlim_max = limit.max;
    }

    if let Some(new_limit) = nullable!(new_limit.get_as_ref())? {
        if new_limit.rlim_cur > new_limit.rlim_max {
            return Err(LinuxError::EINVAL);
        }

        let limit = &mut proc_data.rlim.write()[resource];
        if new_limit.rlim_max <= limit.max {
            limit.max = new_limit.rlim_max;
        } else {
            return Err(LinuxError::EPERM);
        }

        limit.current = new_limit.rlim_cur;
    }

    Ok(0)
}
