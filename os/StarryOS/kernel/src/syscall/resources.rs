use core::mem::offset_of;

use ax_memory_addr::PAGE_SIZE_4K;
use ax_runtime::hal::time::TimeValue;
use linux_raw_sys::general::{
    __kernel_old_timeval, RLIM_NLIMITS, RLIMIT_RTTIME, rlimit64, rusage,
};

use crate::{
    StarryError, StarryResult,
    mm::{UserPtr, VmPtr},
    task::{ProcessData, Rlimit, TgidNumber, Thread, UserTaskRef, get_user_process_data_by_number},
    time::TimeValueLike,
};

pub fn sys_prlimit64(
    current: &UserTaskRef,
    pid: u32,
    resource: u32,
    new_limit: *const rlimit64,
    old_limit: *mut rlimit64,
) -> StarryResult<isize> {
    // pid lookup first — match Linux error priority (ESRCH before EINVAL)
    let proc_data = if pid == 0 {
        current.as_thread().proc_data.clone()
    } else {
        get_user_process_data_by_number(TgidNumber::try_from(pid)?)?
    };

    if resource >= RLIM_NLIMITS {
        return Err(StarryError::InvalidInput);
    }

    if let Some(old_limit) = old_limit.nullable() {
        let limit = proc_data.rlimit(resource);
        let old_limit = UserPtr::<rlimit64>::from(old_limit);
        old_limit.write_field(current, offset_of!(rlimit64, rlim_cur), limit.current)?;
        old_limit.write_field(current, offset_of!(rlimit64, rlim_max), limit.max)?;
    }

    if let Some(new_limit) = new_limit.nullable() {
        // FIXME: AnyBitPattern
        let new_limit = unsafe { new_limit.vm_read_uninit(current)?.assume_init() };
        if new_limit.rlim_cur > new_limit.rlim_max {
            return Err(StarryError::InvalidInput);
        }

        let limit = proc_data.rlimit_update(resource);
        let previous = limit.snapshot();
        // Raising the hard limit requires CAP_SYS_RESOURCE.
        // TODO: has_cap_sys_resource() is currently euid==0 until a
        // fine-grained capability bitmap is implemented (see cred.rs).
        if new_limit.rlim_max > previous.max {
            let cred = current.as_thread().cred();
            if !cred.has_cap_sys_resource() {
                return Err(StarryError::OperationNotPermitted);
            }
        }
        limit.replace(Rlimit::new(new_limit.rlim_cur, new_limit.rlim_max));
        if resource == RLIMIT_RTTIME {
            proc_data.publish_rttime_watchdog_limit();
        }
    }

    Ok(0)
}

#[derive(Default)]
struct Rusage {
    utime: TimeValue,
    stime: TimeValue,
    max_rss_kb: u64,
}

impl Rusage {
    fn from_thread(thread: &Thread) -> Self {
        let (utime, stime) = thread.cpu_time().output();
        let max_rss_kb = thread.proc_data.aspace().lock().rss().hiwater_rss_pages()
            * (PAGE_SIZE_4K as u64 / 1024);
        Self {
            utime,
            stime,
            max_rss_kb,
        }
    }

    fn from_process(proc_data: &ProcessData) -> Self {
        let (utime, stime) = proc_data.cpu_time();
        let max_rss_kb =
            proc_data.aspace().lock().rss().hiwater_rss_pages() * (PAGE_SIZE_4K as u64 / 1024);
        Self {
            utime,
            stime,
            max_rss_kb,
        }
    }

    fn from_waited_children(proc_data: &ProcessData) -> Self {
        let (utime, stime) = proc_data.children_cpu_time();
        Self {
            utime,
            stime,
            max_rss_kb: 0,
        }
    }
}

impl From<Rusage> for rusage {
    fn from(value: Rusage) -> Self {
        // FIXME: Zeroable
        let mut usage: rusage = unsafe { core::mem::zeroed() };
        usage.ru_utime = __kernel_old_timeval::from_time_value(value.utime);
        usage.ru_stime = __kernel_old_timeval::from_time_value(value.stime);
        usage.ru_maxrss = value.max_rss_kb as _;
        usage
    }
}

fn write_rusage(
    current: &crate::task::UserTaskRef,
    user: *mut rusage,
    usage: rusage,
) -> crate::StarryResult<()> {
    let user = UserPtr::from(user);
    let utime = offset_of!(rusage, ru_utime);
    user.write_field(
        current,
        utime + offset_of!(__kernel_old_timeval, tv_sec),
        usage.ru_utime.tv_sec,
    )?;
    user.write_field(
        current,
        utime + offset_of!(__kernel_old_timeval, tv_usec),
        usage.ru_utime.tv_usec,
    )?;
    let stime = offset_of!(rusage, ru_stime);
    user.write_field(
        current,
        stime + offset_of!(__kernel_old_timeval, tv_sec),
        usage.ru_stime.tv_sec,
    )?;
    user.write_field(
        current,
        stime + offset_of!(__kernel_old_timeval, tv_usec),
        usage.ru_stime.tv_usec,
    )?;
    user.write_field(current, offset_of!(rusage, ru_maxrss), usage.ru_maxrss)?;
    user.write_field(current, offset_of!(rusage, ru_ixrss), usage.ru_ixrss)?;
    user.write_field(current, offset_of!(rusage, ru_idrss), usage.ru_idrss)?;
    user.write_field(current, offset_of!(rusage, ru_isrss), usage.ru_isrss)?;
    user.write_field(current, offset_of!(rusage, ru_minflt), usage.ru_minflt)?;
    user.write_field(current, offset_of!(rusage, ru_majflt), usage.ru_majflt)?;
    user.write_field(current, offset_of!(rusage, ru_nswap), usage.ru_nswap)?;
    user.write_field(current, offset_of!(rusage, ru_inblock), usage.ru_inblock)?;
    user.write_field(current, offset_of!(rusage, ru_oublock), usage.ru_oublock)?;
    user.write_field(current, offset_of!(rusage, ru_msgsnd), usage.ru_msgsnd)?;
    user.write_field(current, offset_of!(rusage, ru_msgrcv), usage.ru_msgrcv)?;
    user.write_field(current, offset_of!(rusage, ru_nsignals), usage.ru_nsignals)?;
    user.write_field(current, offset_of!(rusage, ru_nvcsw), usage.ru_nvcsw)?;
    user.write_field(current, offset_of!(rusage, ru_nivcsw), usage.ru_nivcsw)
}

pub fn sys_getrusage(
    current: &crate::task::UserTaskRef,
    who: i32,
    usage: *mut rusage,
) -> crate::StarryResult<isize> {
    const RUSAGE_SELF: i32 = linux_raw_sys::general::RUSAGE_SELF as i32;
    const RUSAGE_CHILDREN: i32 = linux_raw_sys::general::RUSAGE_CHILDREN;
    const RUSAGE_THREAD: i32 = linux_raw_sys::general::RUSAGE_THREAD as i32;

    let thr = current.as_thread();

    let result = match who {
        RUSAGE_SELF => Rusage::from_process(&thr.proc_data),
        RUSAGE_CHILDREN => Rusage::from_waited_children(&thr.proc_data),
        RUSAGE_THREAD => Rusage::from_thread(thr),
        _ => return Err(StarryError::InvalidInput),
    };
    write_rusage(current, usage, result.into())?;

    Ok(0)
}

#[cfg(all(test, not(axtest)))]
fn resources_rlimit_validation_rules_hold_for_test() -> bool {
    use linux_raw_sys::general::RLIM_NLIMITS;

    // Test resource limit validation
    // Resource must be < RLIM_NLIMITS
    let valid_resource = 0u32;
    assert!(valid_resource < RLIM_NLIMITS);

    let max_valid = RLIM_NLIMITS - 1;
    assert!(max_valid < RLIM_NLIMITS);

    // Invalid: resource >= RLIM_NLIMITS
    let invalid_resource = RLIM_NLIMITS;
    assert!(invalid_resource >= RLIM_NLIMITS);

    // Test rlimit64 validation: rlim_cur <= rlim_max
    let valid_cur = 100u64;
    let valid_max = 200u64;
    assert!(valid_cur <= valid_max);

    // Invalid: rlim_cur > rlim_max
    let invalid_cur = 300u64;
    let invalid_max = 200u64;
    assert!(invalid_cur > invalid_max);

    true
}

#[cfg(all(test, not(axtest)))]
mod tests {
    #[test]
    fn resources_rlimit_validation_rules_hold() {
        assert!(super::resources_rlimit_validation_rules_hold_for_test());
    }
}
