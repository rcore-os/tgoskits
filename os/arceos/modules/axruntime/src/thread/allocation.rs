//! Fallible heap storage for unpublished runtime thread resources.
use alloc::{boxed::Box, sync::Arc};

use ax_task::runtime::RuntimeStatus;

pub(super) fn allocation_point() -> Result<(), RuntimeStatus> {
    #[cfg(feature = "fault-injection")]
    ax_task::thread::ThreadAllocationProbe::allocation_point()
        .map_err(|_| RuntimeStatus::NoMemory)?;
    Ok(())
}

pub(super) fn try_box<T>(value: T) -> Result<Box<T>, RuntimeStatus> {
    allocation_point()?;
    Box::try_new(value).map_err(|_| RuntimeStatus::NoMemory)
}

pub(super) fn try_arc<T>(value: T) -> Result<Arc<T>, RuntimeStatus> {
    allocation_point()?;
    Arc::try_new(value).map_err(|_| RuntimeStatus::NoMemory)
}
