use ax_errno::{AxError, AxResult};
use ax_task::current;
use linux_raw_sys::general::{
    CLONE_NEWCGROUP, CLONE_NEWIPC, CLONE_NEWNET, CLONE_NEWNS, CLONE_NEWPID, CLONE_NEWUSER,
    CLONE_NEWUTS,
};

use crate::task::{AsThread, Thread};

/// unshare(2) — disassociate parts of the process execution context
///
/// Supported flags:
/// - `CLONE_NEWUSER` — create a new user namespace (credentials become nobody:65534)
/// - Other namespace flags are accepted as no-ops (StarryOS does not implement full namespaces)
/// - `0` — no-op, always succeeds
///
/// Returns EINVAL for completely unknown flags.
pub fn sys_unshare(flags: i32) -> AxResult<isize> {
    if flags == 0 {
        return Ok(0);
    }

    const SUPPORTED_FLAGS: i32 = CLONE_NEWNS as i32
        | CLONE_NEWCGROUP as i32
        | CLONE_NEWUTS as i32
        | CLONE_NEWIPC as i32
        | CLONE_NEWUSER as i32
        | CLONE_NEWPID as i32
        | CLONE_NEWNET as i32;

    if flags & !SUPPORTED_FLAGS != 0 {
        return Err(AxError::InvalidInput);
    }

    if flags & CLONE_NEWUSER as i32 != 0 {
        let curr = current();
        let thr = curr.as_thread();

        // In a new user namespace, uid/gid start as 65534 (nobody)
        let mut cred = (*thr.cred()).clone();
        cred.uid = 65534;
        cred.gid = 65534;
        cred.euid = 65534;
        cred.egid = 65534;
        cred.suid = 65534;
        cred.sgid = 65534;
        cred.fsuid = 65534;
        cred.fsgid = 65534;

        Thread::set_cred(thr, cred);
    }

    Ok(0)
}
