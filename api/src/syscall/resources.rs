use axerrno::{LinuxError, LinuxResult};
use axhal::time::TimeValue;
use axtask::current;
use linux_raw_sys::general::{__kernel_old_timeval, RLIM_NLIMITS, rlimit64, rusage};
use starry_core::task::{AsThread, Thread, get_process_data, get_task};
use starry_process::Pid;

use crate::{
    mm::{UserConstPtr, UserPtr, nullable},
    time::TimeValueLike,
};

pub fn sys_prlimit64(
    pid: Pid,
    resource: u32,
    new_limit: UserConstPtr<rlimit64>,
    old_limit: UserPtr<rlimit64>,
) -> LinuxResult<isize> {
    if resource >= RLIM_NLIMITS {
        return Err(LinuxError::EINVAL);
    }

    let proc_data = get_process_data(pid)?;
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
            // TODO: patch resources
            // return Err(LinuxError::EPERM);
            return Ok(0);
        }

        limit.current = new_limit.rlim_cur;
    }

    Ok(0)
}

#[derive(Default)]
struct Rusage {
    utime: TimeValue,
    stime: TimeValue,
}

impl Rusage {
    fn from_thread(thread: &Thread) -> Self {
        let (utime, stime) = thread.time.borrow().output();
        Self { utime, stime }
    }

    fn collate(mut self, other: Rusage) -> Self {
        self.utime += other.utime;
        self.stime += other.stime;
        self
    }

    fn to_ctype(&self, usage: &mut rusage) {
        usage.ru_utime = __kernel_old_timeval::from_time_value(self.utime);
        usage.ru_stime = __kernel_old_timeval::from_time_value(self.stime);
    }
}

pub fn sys_getrusage(who: i32, usage: UserPtr<rusage>) -> LinuxResult<isize> {
    const RUSAGE_SELF: i32 = linux_raw_sys::general::RUSAGE_SELF as i32;
    const RUSAGE_CHILDREN: i32 = linux_raw_sys::general::RUSAGE_CHILDREN;
    const RUSAGE_THREAD: i32 = linux_raw_sys::general::RUSAGE_THREAD as i32;

    let curr = current();
    let thr = curr.as_thread();

    let result = match who {
        RUSAGE_SELF => {
            thr.proc_data
                .proc
                .threads()
                .into_iter()
                .fold(Rusage::default(), |acc, tid| {
                    if let Ok(task) = get_task(tid) {
                        acc.collate(Rusage::from_thread(task.as_thread()))
                    } else {
                        acc
                    }
                })
        }
        RUSAGE_CHILDREN => {
            thr.proc_data
                .proc
                .threads()
                .into_iter()
                .fold(Rusage::default(), |acc, child| {
                    if let Ok(task) = get_task(child)
                        && !curr.ptr_eq(&task)
                    {
                        acc.collate(Rusage::from_thread(task.as_thread()))
                    } else {
                        acc
                    }
                })
        }
        RUSAGE_THREAD => Rusage::from_thread(thr),
        _ => return Err(LinuxError::EINVAL),
    };
    result.to_ctype(usage.get_as_mut()?);

    Ok(0)
}
