use alloc::collections::BTreeMap;

use ax_errno::{AxError, AxResult};
use ax_sync::Mutex;
use linux_raw_sys::general::{RLIMIT_AS, RLIMIT_DATA, RLIMIT_NOFILE, rlimit};

use crate::task::AsThread;

/// Resource limit structure
#[derive(Debug, Clone, Copy)]
pub struct ResourceLimit {
    pub current: u64,
    pub maximum: u64,
}

impl Default for ResourceLimit {
    fn default() -> Self {
        Self {
            current: u64::MAX,
            maximum: u64::MAX,
        }
    }
}

/// Global resource limit storage
static RLIMIT_TABLE: Mutex<BTreeMap<u32, ResourceLimit>> = Mutex::new(BTreeMap::new());

/// Initialize default resource limits
fn init_rlimits() {
    let mut limits = RLIMIT_TABLE.lock();
    limits.insert(
        RLIMIT_NOFILE,
        ResourceLimit {
            current: 1024,
            maximum: 1024,
        },
    );
    limits.insert(
        RLIMIT_DATA,
        ResourceLimit {
            current: u64::MAX,
            maximum: u64::MAX,
        },
    );
    limits.insert(
        RLIMIT_AS,
        ResourceLimit {
            current: u64::MAX,
            maximum: u64::MAX,
        },
    );
}

/// Get resource limit
pub fn sys_getrlimit(resource: u32, rlim: *mut rlimit) -> AxResult<isize> {
    info!("sys_getrlimit <= resource: {resource} ({}), rlim: {rlim:p}",
          if resource == 2 { "RLIMIT_DATA" } else if resource == 7 { "RLIMIT_NOFILE" } else { "OTHER" });

    // Check if pointer is null
    if rlim.is_null() {
        warn!("sys_getrlimit: null pointer");
        return Err(AxError::InvalidInput);
    }

    // Ensure limits are initialized
    if RLIMIT_TABLE.lock().is_empty() {
        init_rlimits();
    }

    let limits = RLIMIT_TABLE.lock();
    let limit = limits.get(&resource).copied().unwrap_or_default();

    info!("sys_getrlimit <= found: {}, cur: {}, max: {}",
          limits.contains_key(&resource), limit.current, limit.maximum);

    // SAFETY: The pointer should be valid from userspace
    unsafe {
        (*rlim).rlim_cur = limit.current;
        (*rlim).rlim_max = limit.maximum;
    }

    info!(
        "sys_getrlimit => resource: {resource}, cur: {}, max: {} (OK)",
        limit.current, limit.maximum
    );
    Ok(0)
}

/// Set resource limit
pub fn sys_setrlimit(resource: u32, rlim: *const rlimit) -> AxResult<isize> {
    info!("sys_setrlimit <= resource: {resource}, rlim: {rlim:p}");

    // Check if pointer is null
    if rlim.is_null() {
        warn!("sys_setrlimit: null pointer");
        return Err(AxError::InvalidInput);
    }

    // Ensure limits are initialized
    if RLIMIT_TABLE.lock().is_empty() {
        init_rlimits();
    }

    // SAFETY: The pointer should be valid from userspace
    let (new_cur, new_max) = unsafe {
        let rlim = &*rlim;
        (rlim.rlim_cur, rlim.rlim_max)
    };

    info!("sys_setrlimit <= cur: {new_cur}, max: {new_max}");

    // Allow any reasonable limit values - no validation needed
    let mut limits = RLIMIT_TABLE.lock();
    limits.insert(resource, ResourceLimit {
        current: new_cur,
        maximum: new_max,
    });

    info!(
        "sys_setrlimit => resource: {resource}, cur: {}, max: {} (OK)",
        new_cur, new_max
    );
    Ok(0)
}

/// Check if brk operation would exceed RLIMIT_DATA
pub fn check_brk_rlimit(new_brk: usize) -> AxResult<()> {
    if RLIMIT_TABLE.lock().is_empty() {
        init_rlimits();
    }

    let limits = RLIMIT_TABLE.lock();
    if let Some(data_limit) = limits.get(&RLIMIT_DATA) {
        if data_limit.current != u64::MAX {
            // Calculate current data segment size
            use crate::config::USER_HEAP_BASE;
            let current_data_size = new_brk.saturating_sub(USER_HEAP_BASE);
            if current_data_size as u64 > data_limit.current {
                return Err(AxError::NoMemory);
            }
        }
    }
    Ok(())
}
