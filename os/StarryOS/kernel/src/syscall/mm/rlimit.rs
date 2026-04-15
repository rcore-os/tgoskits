use ax_errno::{AxError, AxResult};
use ax_task::current;
use linux_raw_sys::general::{RLIM_NLIMITS, RLIMIT_AS, RLIMIT_DATA, RLIMIT_NOFILE, rlimit};

use crate::task::AsThread;

/// Get resource limit from per-process resource limits
pub fn sys_getrlimit(resource: u32, rlim: *mut rlimit) -> AxResult<isize> {
    info!(
        "sys_getrlimit <= resource: {resource} ({}), rlim: {rlim:p}",
        if resource == RLIMIT_DATA {
            "RLIMIT_DATA"
        } else if resource == RLIMIT_NOFILE {
            "RLIMIT_NOFILE"
        } else if resource == RLIMIT_AS {
            "RLIMIT_AS"
        } else {
            "OTHER"
        }
    );

    // Check if pointer is null
    if rlim.is_null() {
        warn!("sys_getrlimit: null pointer");
        return Err(AxError::InvalidInput);
    }

    // Get current process
    let current = current();
    let proc_data = &current.as_thread().proc_data;
    let limits = proc_data.rlim.read();

    // Check if resource is valid
    if resource as usize >= RLIM_NLIMITS as usize {
        warn!("sys_getrlimit: invalid resource {resource}");
        return Err(AxError::InvalidInput);
    }

    let limit = &limits[resource];

    info!(
        "sys_getrlimit <= cur: {}, max: {}",
        limit.current, limit.max
    );

    // SAFETY: The pointer should be valid from userspace
    unsafe {
        (*rlim).rlim_cur = limit.current;
        (*rlim).rlim_max = limit.max;
    }

    info!(
        "sys_getrlimit => resource: {resource}, cur: {}, max: {} (OK)",
        limit.current, limit.max
    );
    Ok(0)
}

/// Set resource limit in per-process resource limits
pub fn sys_setrlimit(resource: u32, rlim: *const rlimit) -> AxResult<isize> {
    info!("sys_setrlimit <= resource: {resource}, rlim: {rlim:p}");

    // Check if pointer is null
    if rlim.is_null() {
        warn!("sys_setrlimit: null pointer");
        return Err(AxError::InvalidInput);
    }

    // SAFETY: The pointer should be valid from userspace
    let (new_cur, new_max) = unsafe {
        let rlim = &*rlim;
        (rlim.rlim_cur, rlim.rlim_max)
    };

    info!("sys_setrlimit <= cur: {new_cur}, max: {new_max}");

    // Get current process
    let current = current();
    let proc_data = &current.as_thread().proc_data;
    let mut limits = proc_data.rlim.write();

    // Check if resource is valid
    if resource as usize >= RLIM_NLIMITS as usize {
        warn!("sys_setrlimit: invalid resource {resource}");
        return Err(AxError::InvalidInput);
    }

    // Basic validation: soft limit cannot exceed hard limit
    if new_cur > new_max {
        warn!(
            "sys_setrlimit: soft limit {} exceeds hard limit {}",
            new_cur, new_max
        );
        return Err(AxError::InvalidInput);
    }

    // Update the limit
    limits[resource].current = new_cur;
    limits[resource].max = new_max;

    info!(
        "sys_setrlimit => resource: {resource}, cur: {}, max: {} (OK)",
        new_cur, new_max
    );
    Ok(0)
}

/// Check if brk operation would exceed RLIMIT_DATA
pub fn check_brk_rlimit(new_brk: usize) -> AxResult<()> {
    use linux_raw_sys::general::RLIMIT_DATA;

    let current = current();
    let proc_data = &current.as_thread().proc_data;
    let limits = proc_data.rlim.read();

    let data_limit = &limits[RLIMIT_DATA];
    if data_limit.current != u64::MAX {
        // Calculate current data segment size
        use crate::config::USER_HEAP_BASE;
        let current_data_size = new_brk.saturating_sub(USER_HEAP_BASE);
        if current_data_size as u64 > data_limit.current {
            info!(
                "check_brk_rlimit: data size {} exceeds limit {}",
                current_data_size, data_limit.current
            );
            return Err(AxError::NoMemory);
        }
    }
    Ok(())
}
