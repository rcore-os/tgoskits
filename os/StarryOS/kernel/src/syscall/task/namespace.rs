use alloc::sync::Arc;
use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use ax_task::current;
use linux_raw_sys::general::CLONE_NEWUTS;

use crate::task::AsThread;

/// unshare(2) — disassociate parts of the process execution context.
///
/// Currently only `CLONE_NEWUTS` is supported; other namespace flags return
/// `EINVAL`.
pub fn sys_unshare(flags: u32) -> AxResult<isize> {
    if flags & !CLONE_NEWUTS != 0 {
        debug!("sys_unshare: unsupported flags {:#x}", flags);
        return Err(AxError::InvalidInput);
    }

    if flags & CLONE_NEWUTS != 0 {
        let curr = current();
        let proc_data = &curr.as_thread().proc_data;
        let mut guard = proc_data.uts_ns.lock();
        let new_inner = guard.lock().clone_ns();
        *guard = Arc::new(SpinNoIrq::new(new_inner));
    }

    Ok(0)
}
