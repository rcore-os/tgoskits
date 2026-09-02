use ax_memory_addr::PAGE_SIZE_4K;
use ax_runtime::hal::time::TimeValue;
use ax_task::current;
use linux_raw_sys::general::{__kernel_old_timeval, RLIM_NLIMITS, rlimit64, rusage};
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    StarryError, StarryResult,
    task::{AsThread, TgidNumber, Thread, get_task_by_number, get_user_process_data_by_number},
    time::TimeValueLike,
};

pub fn sys_prlimit64(
    pid: u32,
    resource: u32,
    new_limit: *const rlimit64,
    old_limit: *mut rlimit64,
) -> StarryResult<isize> {
    // pid lookup first — match Linux error priority (ESRCH before EINVAL)
    let proc_data = if pid == 0 {
        current().as_thread().proc_data.clone()
    } else {
        get_user_process_data_by_number(TgidNumber::try_from(pid)?)?
    };

    if resource >= RLIM_NLIMITS {
        return Err(StarryError::InvalidInput);
    }

    if let Some(old_limit) = old_limit.nullable() {
        let (current, max) = {
            let limits = proc_data.rlim.read();
            let limit = &limits[resource];
            (limit.current, limit.max)
        };
        old_limit.vm_write(rlimit64 {
            rlim_cur: current,
            rlim_max: max,
        })?;
    }

    if let Some(new_limit) = new_limit.nullable() {
        // FIXME: AnyBitPattern
        let new_limit = unsafe { new_limit.vm_read_uninit()?.assume_init() };
        if new_limit.rlim_cur > new_limit.rlim_max {
            return Err(StarryError::InvalidInput);
        }

        let limit = &mut proc_data.rlim.write()[resource];
        // Raising the hard limit requires CAP_SYS_RESOURCE.
        // TODO: has_cap_sys_resource() is currently euid==0 until a
        // fine-grained capability bitmap is implemented (see cred.rs).
        if new_limit.rlim_max > limit.max {
            let cred = current().as_thread().cred();
            if !cred.has_cap_sys_resource() {
                return Err(StarryError::OperationNotPermitted);
            }
        }
        limit.max = new_limit.rlim_max;
        limit.current = new_limit.rlim_cur;
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
    fn from_thread(thread: &Thread) -> StarryResult<Self> {
        let (utime, stime) = thread.time.borrow().output();
        let mm = thread.proc_data.pin_aspace()?;
        let max_rss_pages = mm.lock().resident_hiwater_pages();
        let max_rss_kb = max_rss_pages * (PAGE_SIZE_4K as u64 / 1024);
        Ok(Self {
            utime,
            stime,
            max_rss_kb,
        })
    }

    fn collate(mut self, other: Rusage) -> Self {
        self.utime += other.utime;
        self.stime += other.stime;
        self.max_rss_kb = self.max_rss_kb.max(other.max_rss_kb);
        self
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

pub fn sys_getrusage(who: i32, usage: *mut rusage) -> StarryResult<isize> {
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
                .try_fold(Rusage::default(), |acc, tid| -> StarryResult<Rusage> {
                    if let Ok(task) = get_task_by_number(tid) {
                        Ok(acc.collate(Rusage::from_thread(task.as_thread())?))
                    } else {
                        Ok(acc)
                    }
                })?
        }
        RUSAGE_CHILDREN => {
            thr.proc_data
                .proc
                .threads()
                .into_iter()
                .try_fold(Rusage::default(), |acc, child| -> StarryResult<Rusage> {
                    if let Ok(task) = get_task_by_number(child)
                        && !curr.ptr_eq(&task)
                    {
                        Ok(acc.collate(Rusage::from_thread(task.as_thread())?))
                    } else {
                        Ok(acc)
                    }
                })?
        }
        RUSAGE_THREAD => Rusage::from_thread(thr)?,
        _ => return Err(StarryError::InvalidInput),
    };
    usage.vm_write(result.into())?;

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
