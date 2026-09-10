//! Fallible private-thread allocations before Linux identity publication.
use alloc::{sync::Arc, vec::Vec};
use ax_std::os::arceos::task::{runtime::RuntimeStatus, thread::TaskError};

pub(super) fn no_memory() -> TaskError {
    TaskError::RuntimeFailure(RuntimeStatus::NoMemory as u32)
}

pub(super) fn point() -> Result<(), TaskError> {
    #[cfg(axtest)]
    ax_std::os::arceos::task::thread::ThreadAllocationProbe::allocation_point()?;
    Ok(())
}

pub(super) fn try_arc<T>(value: T) -> Result<Arc<T>, TaskError> {
    point()?;
    Arc::try_new(value).map_err(|_| no_memory())
}

pub(super) fn try_vec<T>(capacity: usize) -> Result<Vec<T>, TaskError> {
    point()?;
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| no_memory())?;
    Ok(values)
}
